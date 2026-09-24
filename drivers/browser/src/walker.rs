//! DOM → Element mapping. `WALKER_JS` runs inside the page and emits a
//! flat JSON array of semantic nodes; `parse_elements` turns it into
//! `dexter_core::Element`s. The script also stores
//! `window.__dexterNodes` — live DOM node references indexed by
//! `ElementId`, which is how `act()` reaches nodes without coordinates.

use dexter_core::{Element, ElementId, ElementSource, Rect};
use serde::Deserialize;

/// The in-page walker. Returns `{elements:[{id,parent,depth,role,name,
/// value,bounds,enabled,focused,actions}], errors}` and stashes the live
/// nodes in `window.__dexterNodes` for later action dispatch.
///
/// Same-origin iframes are walked transparently: the iframe element
/// becomes a `web_area` node whose descendants carry bounds offset by
/// the iframe's rect. Cross-origin (or unloaded) frames still appear as
/// `web_area` elements but increment `errors` — content we cannot see
/// is reported, never silently dropped.
pub const WALKER_JS: &str = r#"
return (() => {
  const els = [];
  const nodes = [];
  const idx = new Map(); // Node -> flat index
  let errors = 0;

  const ROLE_BY_TAG = {
    a: 'link', button: 'button', select: 'combo_box',
    textarea: 'text_area', summary: 'button', details: 'disclosure',
    h1: 'heading', h2: 'heading', h3: 'heading',
    h4: 'heading', h5: 'heading', h6: 'heading',
    nav: 'navigation', main: 'main', aside: 'complementary',
    form: 'form', table: 'table', tr: 'row', td: 'cell', th: 'cell',
    ul: 'list', ol: 'list', li: 'list_item', img: 'image',
    label: 'static_text', p: 'static_text', span: 'static_text',
    dialog: 'dialog', fieldset: 'group', legend: 'static_text',
    iframe: 'web_area',
  };
  const INPUT_ROLES = {
    checkbox: 'check_box', radio: 'radio_button', range: 'slider',
    number: 'text_field', text: 'text_field', search: 'text_field',
    email: 'text_field', password: 'text_field', tel: 'text_field',
    url: 'text_field', date: 'text_field', time: 'text_field',
    submit: 'button', button: 'button', reset: 'button',
    image: 'button', file: 'button', color: 'button', hidden: null,
  };
  const TEXT_TAGS = new Set(['h1','h2','h3','h4','h5','h6','p','label','li','td','th','legend','caption','figcaption','title']);

  function roleOf(el) {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'input') return INPUT_ROLES[(el.type || 'text').toLowerCase()] ?? 'text_field';
    return ROLE_BY_TAG[tag] || 'generic';
  }

  function accName(el) {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
      const parts = labelledBy.split(/\s+/).map(id => document.getElementById(id))
        .filter(Boolean).map(n => n.textContent.trim());
      if (parts.length) return parts.join(' ');
    }
    if (el.labels && el.labels.length) return el.labels[0].textContent.trim();
    const ph = el.getAttribute('placeholder');
    if (ph) return ph;
    const alt = el.getAttribute('alt');
    if (alt) return alt;
    const title = el.getAttribute('title');
    if (title) return title;
    const tag = el.tagName.toLowerCase();
    // For AX, the accessible name of a button/link IS its label; for
    // static text it IS the text content.
    if (['button','a','summary','option'].includes(tag) || TEXT_TAGS.has(tag)) {
      const t = (el.innerText || el.textContent || '').trim();
      return t.length > 120 ? t.slice(0, 120) : t;
    }
    return '';
  }

  function actionsOf(el, role) {
    const a = [];
    const tag = el.tagName.toLowerCase();
    const interactive =
      ['a','button','select','summary','option'].includes(tag) ||
      tag === 'input' ||
      el.onclick != null || el.getAttribute('tabindex') != null ||
      ['button','link','check_box','radio_button','combo_box','tab','menu_item','switch'].includes(role);
    if (interactive) a.push('press');
    if (tag === 'input' || tag === 'textarea' || el.isContentEditable ||
        ['text_field','text_area','combo_box'].includes(role)) {
      a.push('set_value');
      a.push('focus');
    } else if (el.getAttribute('tabindex') != null) {
      a.push('focus');
    }
    a.push('scroll_into_view');
    return a;
  }

  function visible(el) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const style = getComputedStyle(el);
    return style.display !== 'none' && style.visibility !== 'hidden';
  }

  function push(el, parentIdx, depth, ox, oy) {
    const i = els.length;
    const r = el.getBoundingClientRect();
    const role = roleOf(el);
    const name = accName(el);
    const tag = el.tagName.toLowerCase();
    let value = null;
    // Sensitive fields never materialize their value — same redaction
    // contract the AX path enforces on secure fields.
    const sensitive = tag === 'input' && (el.type || '').toLowerCase() === 'password';
    if (!sensitive && (tag === 'input' || tag === 'textarea' || tag === 'select')) {
      value = el.type === 'checkbox' || el.type === 'radio'
        ? String(el.checked) : el.value;
    } else if (TEXT_TAGS.has(tag)) {
      const t = (el.innerText || '').trim();
      value = t.length > 200 ? t.slice(0, 200) : (t || null);
    } else if (el.getAttribute('aria-valuenow')) {
      value = el.getAttribute('aria-valuenow');
    }
    els.push({
      id: i,
      parent: parentIdx,
      depth,
      role,
      raw_role: el.getAttribute('role') || tag,
      subrole: sensitive ? 'password' : null,
      name: name || null,
      value,
      bounds: { x: r.x + ox, y: r.y + oy, w: r.width, h: r.height },
      enabled: !el.disabled && el.getAttribute('aria-disabled') !== 'true',
      focused: document.activeElement === el,
      actions: actionsOf(el, role),
      identifier: el.id || null,
    });
    idx.set(el, i);
    nodes.push(el);
    return i;
  }

  // Walk elements that carry semantics: interactive, named, or textful.
  const SKIP = new Set(['script','style','noscript','template','svg','path','meta','link','head','br','hr']);
  // `ox`/`oy` accumulate iframe offsets — getBoundingClientRect inside a
  // frame is relative to that frame's viewport.
  function walk(node, parentIdx, depth, ox, oy) {
    if (depth > 24 || els.length >= 4000) return;
    for (const child of node.children) {
      const tag = child.tagName.toLowerCase();
      if (SKIP.has(tag) || !visible(child)) continue;
      if (tag === 'iframe') {
        const fr = child.getBoundingClientRect();
        const fIdx = push(child, parentIdx, depth, ox, oy);
        try {
          const doc = child.contentDocument;
          if (doc && (doc.body || doc.documentElement)) {
            walk(doc, fIdx, depth + 1, ox + fr.x, oy + fr.y);
          } else {
            // Cross-origin or unloaded — content we cannot see.
            errors++;
          }
        } catch (e) {
          errors++;
        }
        continue;
      }
      const semantic =
        ['a','button','input','select','textarea','summary','option','label','img'].includes(tag) ||
        child.getAttribute('role') != null ||
        child.onclick != null || child.getAttribute('tabindex') != null ||
        child.isContentEditable ||
        (TEXT_TAGS.has(tag) && (child.innerText || '').trim().length > 0) ||
        ['main','nav','aside','form','table','ul','ol','dialog','fieldset','details'].includes(tag);
      let myIdx = parentIdx;
      let myDepth = depth;
      if (semantic) {
        myIdx = push(child, parentIdx, depth, ox, oy);
        myDepth = depth + 1;
      }
      walk(child, myIdx, myDepth, ox, oy);
    }
  }
  walk(document.body || document.documentElement, null, 0, 0, 0);
  window.__dexterNodes = nodes;
  return { elements: els, errors };
})()
"#;

/// One flat node as emitted by `WALKER_JS`.
#[derive(Debug, Deserialize)]
struct RawElement {
    id: usize,
    parent: Option<usize>,
    depth: u32,
    role: String,
    raw_role: Option<String>,
    subrole: Option<String>,
    name: Option<String>,
    value: Option<String>,
    bounds: Option<RawRect>,
    enabled: Option<bool>,
    focused: bool,
    actions: Vec<String>,
    identifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Debug, Deserialize)]
struct WalkResult {
    elements: Vec<RawElement>,
    #[serde(default)]
    errors: u32,
}

/// Parse the walker's JSON output into core `Element`s plus the number
/// of frames that could not be walked (cross-origin/unloaded).
pub fn parse_elements(raw: serde_json::Value) -> (Vec<Element>, u32) {
    let res: WalkResult = match serde_json::from_value(raw) {
        Ok(v) => v,
        Err(_) => return (Vec::new(), 0),
    };
    let elements = res
        .elements
        .into_iter()
        .map(|r| Element {
            id: ElementId(r.id as u64),
            parent: r.parent.map(|p| ElementId(p as u64)),
            depth: r.depth,
            role: Some(r.role),
            raw_role: r.raw_role,
            subrole: r.subrole,
            name: r.name,
            value: r.value,
            bounds: r.bounds.map(|b| Rect {
                x: b.x,
                y: b.y,
                w: b.w,
                h: b.h,
            }),
            enabled: r.enabled,
            focused: r.focused,
            actions: r.actions,
            identifier: r.identifier,
            // DOM accesskey modifiers are platform-dependent and not
            // reliably mappable to a KeyChord — left unset.
            shortcut: None,
            source: ElementSource::Dom,
        })
        .collect();
    (elements, res.errors)
}
