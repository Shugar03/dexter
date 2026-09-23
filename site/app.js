/* dexter landing — presence animation + scroll reveals */
gsap.registerPlugin(ScrollTrigger);

const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/* ---- hero presence cursor: a tiny journal, replayed forever ---- */
(function () {
  const cursor = document.getElementById("hp-cursor");
  const tag = document.getElementById("hp-tag");
  const reticle = document.getElementById("hp-reticle");
  const email = document.getElementById("hp-email");
  const card = document.getElementById("hp-card");
  const pay = document.getElementById("hp-pay");
  if (!cursor || !email || !card || !pay) return;

  function rectOf(el, pad) {
    const w = el.closest(".win").getBoundingClientRect();
    const r = el.getBoundingClientRect();
    return {
      x: r.left - w.left - (pad || 0),
      y: r.top - w.top - (pad || 0),
      w: r.width + (pad || 0) * 2,
      h: r.height + (pad || 0) * 2,
    };
  }
  function flyTo(el, tagText) {
    const r = rectOf(el);
    return gsap.to(cursor, {
      x: r.x + r.w / 2, y: r.y + r.h / 2, duration: 0.55, ease: "power2.inOut",
      onStart: () => { tag.textContent = tagText; },
    });
  }
  function flash(el) {
    return gsap.fromTo(el, { scale: 1 }, { scale: 1.02, yoyo: true, repeat: 1, duration: 0.09 });
  }
  function typeInto(el, text) {
    const tl = gsap.timeline();
    tl.call(() => el.classList.add("hot"));
    for (const ch of text) {
      tl.call(() => {
        let s = el.querySelector(".f-typed");
        if (!s) { s = document.createElement("span"); s.className = "f-typed"; el.appendChild(s); }
        s.textContent += ch;
      });
      tl.to({}, { duration: 0.045 });
    }
    return tl;
  }

  function buildTimeline() {
    const tl = gsap.timeline({ repeat: -1, repeatDelay: 1.6 });
    tl.set(cursor, { x: 40, y: 110 });
    tl.set(reticle, { opacity: 0 });
    tl.call(() => { tag.textContent = "dexter · observing"; });

    tl.to({}, { duration: 0.6 });
    tl.add(flyTo(email, "dexter · typing e_3"));
    tl.add(typeInto(email, "agent@dexter.dev"));
    tl.add(flyTo(card, "dexter · typing e_4"));
    tl.add(typeInto(card, "4242 4242 4242 4242"));
    tl.add(flyTo(pay, "dexter · clicking e_5"));
    tl.call(() => {
      const r = rectOf(pay, 5);
      gsap.set(reticle, { x: r.x, y: r.y, width: r.w, height: r.h });
    });
    tl.to(reticle, { opacity: 1, duration: 0.18 });
    tl.add(flash(pay));
    tl.to(pay, { duration: 0.01, onStart: () => pay.classList.add("hot") });
    tl.call(() => { tag.textContent = "dexter · verifying"; });
    tl.to({}, { duration: 0.7 });
    tl.call(() => { tag.textContent = "dexter · verified ✓"; tag.style.background = "#30a46c"; });
    tl.to({}, { duration: 1.4 });

    // reset
    tl.to(reticle, { opacity: 0, duration: 0.2 });
    tl.call(() => {
      tag.style.background = "";
      email.classList.remove("hot"); card.classList.remove("hot"); pay.classList.remove("hot");
      email.querySelectorAll(".f-typed").forEach((n) => n.remove());
      card.querySelectorAll(".f-typed").forEach((n) => n.remove());
    });
    return tl;
  }

  if (reduced) {
    // Static composed state: cursor on Pay, verified.
    const r = rectOf(pay);
    gsap.set(cursor, { x: r.x + r.w / 2, y: r.y + r.h / 2 });
    tag.textContent = "dexter · verified ✓";
    tag.style.background = "#30a46c";
    return;
  }
  // build after fonts/layout settle; rebuild on resize so rects stay true
  let tl;
  const start = () => { if (tl) tl.kill(); tl = buildTimeline(); };
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(start);
  else start();
  let rt;
  window.addEventListener("resize", () => { clearTimeout(rt); rt = setTimeout(start, 250); });
})();

/* ---- scroll reveals ---- */
if (!reduced) {
  gsap.utils.toArray(".reveal").forEach((el) => {
    gsap.to(el, {
      opacity: 1, y: 0, duration: 0.7, ease: "power2.out",
      scrollTrigger: { trigger: el, start: "top 86%", once: true },
    });
  });
}
