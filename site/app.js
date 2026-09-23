/* dexter — the page observes itself.
   The obs card digests the real DOM, e_N outlines mark real elements,
   and the presence cursor performs on them — the overlay concept,
   on the product's own surface. */

(() => {
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const hasGsap = typeof gsap !== "undefined";
  if (reduced || !hasGsap) document.documentElement.classList.add("no-anim");
  if (hasGsap && typeof ScrollTrigger !== "undefined") gsap.registerPlugin(ScrollTrigger);

  /* ---------- 1. observe the real DOM ---------- */
  const OBSERVED = [
    ...document.querySelectorAll(
      ".nav .brand, .nav-links a, .nav .btn, .hero-ctas a, .artifact-copy a, .foot-cta .btn"
    ),
  ];

  const roleOf = (el) =>
    el.tagName === "A" ? "link" : el.tagName === "BUTTON" ? "button" : "element";

  const elements = OBSERVED.filter((el) => {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }).map((el, i) => {
    const r = el.getBoundingClientRect();
    return {
      id: `e_${i + 1}`,
      el,
      role: roleOf(el),
      name: (el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 40),
      rect: { x: Math.round(r.x), y: Math.round(r.y + scrollY), w: Math.round(r.width), h: Math.round(r.height) },
    };
  });

  const digest = document.getElementById("obs-digest");
  const status = document.getElementById("obs-status");
  const app = "dexter.landing";

  const lines = [
    `observation 14 of ${app} (1 windows, ${elements.length} elements)`,
    `window 1 "${document.title.slice(0, 42)}" [0,0,${innerWidth}x${innerHeight}] focused`,
    ...elements.map(
      (e) =>
        `${e.id} ${e.role} "${e.name}" enabled actions=[click] [${e.rect.x},${e.rect.y},${e.rect.w}x${e.rect.h}]`
    ),
  ];

  const paint = (n) => {
    digest.innerHTML = lines
      .slice(0, n)
      .map((l, i) => {
        if (i === 0) return `<span class="k">${l}</span>`;
        if (i === 1) return `<span class="k">${l}</span>`;
        return l.replace(/^(e_\d+)/, '<span class="tag">$1</span>');
      })
      .join("\n");
  };

  /* ---------- 2. e_N outlines on real elements ---------- */
  const outlines = elements.map((e) => {
    const box = document.createElement("div");
    box.className = "obs-outline";
    box.style.cssText = "position:fixed;pointer-events:none";
    const tag = document.createElement("span");
    tag.className = "obs-eid";
    tag.textContent = e.id;
    box.appendChild(tag);
    document.body.appendChild(box);
    return { e, box };
  });

  const trackOutlines = () => {
    outlines.forEach(({ e, box }) => {
      const r = e.el.getBoundingClientRect();
      box.style.left = r.x - 4 + "px";
      box.style.top = r.y - 4 + "px";
      box.style.width = r.width + 8 + "px";
      box.style.height = r.height + 8 + "px";
      box.style.opacity = r.bottom < 0 || r.top > innerHeight ? 0 : 0.85;
    });
  };

  /* ---------- 3. presence cursor ---------- */
  const cursor = document.getElementById("presence");
  const tag = document.getElementById("presence-tag");
  let pulseEl = null;

  const pulse = (el) => {
    if (pulseEl) pulseEl.remove();
    const r = el.getBoundingClientRect();
    pulseEl = document.createElement("div");
    pulseEl.style.cssText = `position:fixed;left:${r.x - 4}px;top:${r.y - 4}px;width:${r.width + 8}px;height:${r.height + 8}px;border:1.5px solid var(--red);border-radius:8px;pointer-events:none;z-index:59`;
    document.body.appendChild(pulseEl);
    gsap.fromTo(pulseEl, { opacity: 0.9, scale: 0.96 }, { opacity: 0, scale: 1.12, duration: 0.7, ease: "power2.out", onComplete: () => { pulseEl?.remove(); pulseEl = null; } });
  };

  const visit = (e, act) => {
    const r = e.el.getBoundingClientRect();
    const cx = r.x + r.width / 2;
    const cy = r.y + r.height / 2;
    tag.textContent = `dexter · ${act} ${e.id}`;
    return gsap.to(cursor, { x: cx - 4, y: cy - 6, duration: 0.9, ease: "power3.inOut" })
      .then(() => { if (act === "click") pulse(e.el); });
  };

  const tour = () => {
    const tl = gsap.timeline({ delay: 0.4 });
    // first pass: tour the nav + hero CTAs
    elements.slice(0, 6).forEach((e) => {
      tl.call(() => visit(e, "hover"));
      tl.to({}, { duration: 1.1 });
    });
    // then act on the github CTA
    const gh = elements.find((e) => e.name.toLowerCase().includes("github"));
    if (gh) {
      tl.call(() => visit(gh, "click"));
      tl.to({}, { duration: 1.6 });
    }
    // idle revisit loop
    tl.call(function loop() {
      const e = elements[Math.floor(Math.random() * elements.length)];
      const acts = ["hover", "hover", "click"];
      visit(e, acts[Math.floor(Math.random() * acts.length)]).then(() =>
        gsap.delayedCall(2.2 + Math.random() * 2, loop)
      );
    });
  };

  /* ---------- 4. intro sequence ---------- */
  const startObserved = () => {
    // digest types out, then outlines appear
    const total = lines.length;
    let shown = 0;
    const type = setInterval(() => {
      shown += 1;
      paint(shown);
      status.textContent = `observation 14 · ${Math.min(shown - 2, elements.length)}/${elements.length} elements`;
      if (shown >= total) {
        clearInterval(type);
        status.textContent = `observation 14 · ${elements.length} elements · live`;
        trackOutlines();
        outlines.forEach(({ box }, i) => gsap.to(box, { opacity: 0.85, duration: 0.3, delay: i * 0.05 }));
        gsap.to(cursor, { opacity: 1, duration: 0.4, delay: 0.5, onComplete: tour });
      }
    }, 90);
  };

  if (reduced || !hasGsap) {
    paint(lines.length);
    status.textContent = `observation 14 · ${elements.length} elements`;
  } else {
    // headline lines
    gsap.to(".hero .line > span", { y: 0, duration: 1.0, ease: "power4.out", stagger: 0.12, delay: 0.15 });
    gsap.from(".hero-sub, .hero-ctas", { opacity: 0, y: 20, duration: 0.8, delay: 0.6, stagger: 0.1 });
    gsap.from(".obs-card", { opacity: 0, y: 32, duration: 0.9, delay: 0.45, onComplete: startObserved });
    cursor.style.transform = `translate(${innerWidth * 0.6}px, ${innerHeight * 0.5}px)`;

    // statement + footer line reveals
    [".statement", ".foot-cta"].forEach((sel) => {
      const spans = document.querySelectorAll(`${sel} .line > span`);
      if (spans.length)
        gsap.to(spans, {
          y: 0, duration: 1.0, ease: "power4.out", stagger: 0.12,
          scrollTrigger: { trigger: sel, start: "top 75%" },
        });
    });

    // journal feed writes itself on scroll — the way it does mid-run
    document.querySelectorAll(".jline").forEach((el) => {
      gsap.to(el, {
        opacity: el.classList.contains("jline-dim") ? 0.55 : 1,
        y: 0, duration: 0.5, ease: "power2.out",
        scrollTrigger: { trigger: el, start: "top 92%" },
      });
    });

    // generic reveals
    document.querySelectorAll(".reveal").forEach((el, i) => {
      gsap.to(el, {
        opacity: 1, y: 0, duration: 0.8, ease: "power3.out",
        scrollTrigger: { trigger: el, start: "top 88%" },
        delay: (i % 4) * 0.07,
      });
    });
  }

  addEventListener("scroll", trackOutlines, { passive: true });
  addEventListener("resize", trackOutlines);
})();
