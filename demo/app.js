/* dexter demo — GSAP scene engine.
   Telemetry embedded from a real journal: `dexter task "pay the order"`
   against live Chrome (WebDriver/DOM). Event kinds, ordering, priors and
   element ids are verbatim; timestamps are normalized to ms offsets. */

gsap.registerPlugin(ScrollTrigger);

const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/* ---------- real journal, captured from a live task run ---------- */
const JOURNAL = [
  { t: 0,    k: "ObservationCreated",   d: "5 elements" },
  { t: 0,    k: "CandidatesGenerated",  d: "1 ranked · goal “pay the order”" },
  { t: 0,    k: "DecisionMade",         d: "act → Pay $49.00" },
  { t: 0,    k: "ActionProposed",       d: "click “Pay $49.00”" },
  { t: 0,    k: "HumanApprovalRequired",d: "fingerprint gate" },
  { t: 0,    k: "PolicyChecked",        d: "approved" },
  { t: 6,    k: "ActionExecuted",       d: "clicked element e_4 · Dom" },
  { t: 10,   k: "ObservationCreated",   d: "4 elements" },
  { t: 10,   k: "TaskCompleted",        d: "1 step · 15ms" },
];

const KIND_CLASS = {
  ObservationCreated: "k-obs", CandidatesGenerated: "k-cand",
  DecisionMade: "k-dec", ActionProposed: "k-dec",
  HumanApprovalRequired: "k-pol", PolicyChecked: "k-pol",
  ActionExecuted: "k-act", VerificationPassed: "k-ver",
  TaskCompleted: "k-done", TaskFailed: "k-fail",
};

/* ---------- nav status ---------- */
const navDot = document.getElementById("navDot");
const navStatus = document.getElementById("navStatus");
function setStatus(txt, live) {
  navStatus.textContent = txt;
  navDot.classList.toggle("live", !!live);
}

/* ---------- journal feed (bento) ---------- */
const feed = document.getElementById("journalFeed");
JOURNAL.forEach((e) => {
  const li = document.createElement("li");
  li.className = KIND_CLASS[e.k] || "";
  li.innerHTML =
    `<span class="t">+${String(e.t).padStart(3, "0")}ms</span>` +
    `<span class="k">${e.k}</span><span class="d">${e.d}</span>`;
  feed.appendChild(li);
});

/* element tree — from the real state_digest of that same observation */
document.getElementById("treeFeed").innerHTML = [
  `<span class="id">e_0</span> main`,
  `  <span class="id">e_1</span> heading "dexter demo"`,
  `  <span class="id">e_2</span> static_text "CARD NUMBER"`,
  `  <span class="id">e_3</span> text_field "Card number"`,
  `  <span class="id hl">e_4</span> <span class="hl">button "Pay $49.00"</span>`,
].join("\n");

/* candidate bars animate to their real priors */
document.querySelectorAll(".cand").forEach((c) => {
  const p = parseFloat(c.dataset.p);
  c.querySelector(".cand-bar").style.setProperty("--p", 0);
  c._prior = p;
});

/* ==================================================================
   SCENE TIMELINES — each is an autonomous GSAP timeline, played only
   while its scene is on screen (ScrollTrigger toggles, fully killed on
   teardown). Reduced motion: render the end state, skip loops.
   ================================================================== */

function pulseCursor(cursorEl) {
  return gsap.fromTo(cursorEl, { scale: 1 }, {
    scale: 0.78, duration: 0.12, yoyo: true, repeat: 1, ease: "power2.inOut",
  });
}

function scan(scanEl, { fail = false, y = "40%" } = {}) {
  scanEl.classList.toggle("fail", fail);
  return gsap.fromTo(scanEl,
    { top: "0%", opacity: 1 },
    { top: y, opacity: 0, duration: 0.7, ease: "power1.in" });
}

function setTag(tagId, state) {
  const el = document.getElementById(tagId);
  if (el) el.innerHTML = `dexter <span class="st">· ${state}</span>`;
}

function moveCursorTo(cursorEl, targetEl, vp, pad = 0.5) {
  /* offsetLeft/Top are layout values — immune to the scale transforms
     ScrollTrigger applies to ancestor scene cards (getBoundingClientRect
     is not, and produced misplaced reticles mid-scrub). */
  return {
    x: targetEl.offsetLeft + targetEl.offsetWidth * pad,
    y: targetEl.offsetTop + targetEl.offsetHeight * 0.5,
  };
}

function rectAround(el, padX, padY) {
  return {
    left: el.offsetLeft - padX,
    top: el.offsetTop - padY,
    width: el.offsetWidth + padX * 2,
    height: el.offsetHeight + padY * 2,
  };
}

/* ---------- hero mini-viewport loop ---------- */
function heroLoop() {
  const vp = document.getElementById("heroVp");
  const cursor = document.getElementById("heroCursor");
  const ret = document.getElementById("heroReticle");
  const scanEl = document.getElementById("heroScan");
  const btn = document.getElementById("heroBtn");
  const field = document.getElementById("heroField");
  const hud = document.getElementById("heroHud");

  const tl = gsap.timeline({ repeat: -1, repeatDelay: 1.1 });
  const p1 = () => moveCursorTo(cursor, field, vp, 0.6);
  const p2 = () => moveCursorTo(cursor, btn, vp, 0.5);

  tl.call(() => { hud.textContent = "observing…"; ret.style.opacity = 0; setTag("heroTag", "observing"); })
    .to(cursor, { left: "18%", top: "72%", duration: 0.01 })
    .to(cursor, {
      duration: 0.9, ease: "power2.inOut",
      onStart: () => { const p = p1(); gsap.set(cursor, { left: p.x, top: p.y, xPercent: 0, yPercent: 0 }); },
      left: () => p1().x, top: () => p1().y,
    })
    .call(() => { hud.textContent = "e_3 text_field — focus + set_value"; field.style.borderColor = "rgba(77,227,255,.6)"; setTag("heroTag", "typing e_3"); })
    .to({}, { duration: 0.7 })
    .call(() => { hud.textContent = "ranking candidates…"; })
    .to(cursor, {
      duration: 0.8, ease: "power3.inOut",
      left: () => p2().x, top: () => p2().y,
    })
    .call(() => {
      hud.textContent = "lock → e_4 button “Pay $49.00” · 0.72";
      setTag("heroTag", "locking e_4");
      gsap.set(ret, { ...rectAround(btn, 10, 8), opacity: 1 });
    })
    .fromTo(ret, { scale: 1.5, opacity: 0 }, { scale: 1, opacity: 1, duration: 0.35, ease: "back.out(2)" })
    .add(pulseCursor(cursor))
    .call(() => { hud.textContent = "policy → approved · act: dom.click"; setTag("heroTag", "clicking e_4"); })
    .add(() => scan(scanEl, { y: "80%" }))
    .to({}, { duration: 0.8 })
    .call(() => { hud.textContent = "verified — “Payment confirmed”"; setTag("heroTag", "verified ✓"); })
    .to(ret, { opacity: 0, duration: 0.4 }, "+=0.7")
    .call(() => { field.style.borderColor = ""; });
  return tl;
}

/* ---------- scene 1 — semantic target lock ---------- */
function lockTimeline() {
  const vp = document.getElementById("lockVp");
  const cursor = document.getElementById("lockCursor");
  const ret = document.getElementById("lockReticle");
  const tag = document.getElementById("retTag");
  const scanEl = document.getElementById("lockScan");
  const els = ["lockElA", "lockElB", "lockElC"].map((id) => document.getElementById(id));
  const hud = document.getElementById("lockHud");

  const hudRows = [
    ["goal", "exportar el documento"],
    ["candidates", "3 ranked"],
    ["selected", "e_40 · 0.81"],
    ["policy", "approved"],
    ["verify", "text_present ✓"],
  ];
  hud.innerHTML = hudRows.map(([k, v]) =>
    `<li><span>${k}</span><span class="v">${v}</span></li>`).join("");

  const priors = ["0.18", "0.81", "0.22"];
  els.forEach((el, i) => {
    el.dataset.prior = priors[i];
  });

  const tl = gsap.timeline({ repeat: -1, repeatDelay: 1.4 });

  tl.call(() => setTag("lockTag", "observing"), null, 0.1);
  /* sweep: each candidate lights with its prior chip */
  els.forEach((el, i) => {
    tl.call(() => el.classList.add("cand"), null, 0.4 + i * 0.35);
  });

  /* cursor glides to the winner (e_40 — prior 0.81) */
  tl.to(cursor, {
    duration: 1.1, ease: "power3.inOut",
    left: () => moveCursorTo(cursor, els[1], vp, 0.5).x,
    top: () => moveCursorTo(cursor, els[1], vp, 0.5).y,
  }, 1.3);

  /* reticle contracts on the winner — Fitts: target becomes huge */
  tl.call(() => {
    tag.textContent = "e_40 · 0.81";
    gsap.set(ret, rectAround(els[1], 12, 10));
  }, null, 2.4)
    .fromTo(ret, { scale: 1.9, opacity: 0 }, { scale: 1, opacity: 1, duration: 0.45, ease: "back.out(2.2)" }, 2.45)
    .call(() => { els[1].classList.add("locked"); setTag("lockTag", "locked e_40"); }, null, 2.9)
    .call(() => { els[0].classList.remove("cand"); els[2].classList.remove("cand"); }, null, 2.9)

    /* click pulse → verify scan */
    .add(pulseCursor(cursor), 3.1)
    .call(() => setTag("lockTag", "clicking"), null, 3.1)
    .add(() => scan(scanEl, { y: "90%" }), 3.35)
    .call(() => setTag("lockTag", "verified ✓"), null, 4.2)
    .to({}, { duration: 0.9 })

    /* reset for loop */
    .call(() => {
      els.forEach((e) => e.classList.remove("cand", "locked"));
      ret.style.opacity = 0;
    }, null, "+=0.6")
    .to(cursor, { left: "12%", top: "80%", duration: 0.7, ease: "power2.inOut" });

  return tl;
}

/* ---------- scene 2 — flight path ---------- */
function flightTimeline() {
  const vp = document.getElementById("deskVp");
  const cursor = document.getElementById("deskCursor");
  const path = document.getElementById("trailPath");
  const front = document.getElementById("deskFront");
  const btn = document.getElementById("deskBtn");
  const steps = [...document.querySelectorAll("#pipeline li")];
  const len = path.getTotalLength();

  gsap.set(path, { strokeDasharray: len, strokeDashoffset: len });
  gsap.set(["#wp1", "#wp2"], { opacity: 0, scale: 0, transformOrigin: "center" });

  const tl = gsap.timeline({ repeat: -1, repeatDelay: 1.5 });

  tl.call(() => setTag("deskTag", "working"), null, 0.1);
  /* pipeline lights in journal order as the cursor travels */
  const stepAt = (i) => tl.call(() => {
    steps.forEach((s, j) => s.classList.toggle("on", j <= i));
  }, null, 0.3 + i * 0.55);
  steps.forEach((_, i) => stepAt(i));

  /* cursor draws the trail across the desktop into the front window */
  tl.to(path, { strokeDashoffset: 0, duration: 2.6, ease: "power1.inOut" }, 0.2)
    .to(cursor, {
      duration: 2.6, ease: "power1.inOut",
      motionPath: null, /* path is decorative; cursor follows keyframes */
      keyframes: [
        { left: "12%", top: "82%" },
        { left: "40%", top: "56%" },
        { left: "68%", top: "56%" },
      ],
    }, 0.2)
    .to("#wp1", { opacity: 1, scale: 1, duration: 0.3 }, 1.0)
    .to("#wp2", { opacity: 1, scale: 1, duration: 0.3 }, 2.4)
    /* front window pulses — the agent's working surface */
    .fromTo(front, { boxShadow: "0 0 0 1px rgba(77,227,255,0.2), 0 24px 60px rgba(0,0,0,.6)" },
      { boxShadow: "0 0 0 2px rgba(77,227,255,0.55), 0 24px 80px rgba(77,227,255,.12)", duration: 0.5 }, 2.6)
    .add(pulseCursor(cursor), 3.2)
    .to(btn, { scale: 0.96, duration: 0.1, yoyo: true, repeat: 1 }, 3.2)
    .to({}, { duration: 1.0 })
    .call(() => {
      steps.forEach((s) => s.classList.remove("on"));
      gsap.set(path, { strokeDashoffset: len });
      gsap.set(["#wp1", "#wp2"], { opacity: 0, scale: 0 });
    });

  return tl;
}

/* ---------- scene 3 — verify or recover ---------- */
function verifyTimeline() {
  const vp = document.getElementById("verifyVp");
  const cursor = document.getElementById("verCursor");
  const target = document.getElementById("verTarget");
  const status = document.getElementById("verStatus");
  const scanEl = document.getElementById("verScan");
  const flash = document.getElementById("verFlash");
  const brRetry = document.getElementById("brRetry");
  const brAbstain = document.getElementById("brAbstain");

  const setStatus = (cls, txt, tag) => () => {
    status.className = "ver-status mono " + cls;
    status.textContent = txt;
    if (tag) setTag("verTag", tag);
  };

  const tl = gsap.timeline({ repeat: -1, repeatDelay: 1.6 });

  /* ACT 1 — click → verify FAILS → retry → verified */
  tl.call(setStatus("proposed", "proposed — click e_4", "acting"), null, 0.2)
    .to(cursor, {
      duration: 0.9, ease: "power3.inOut",
      left: () => moveCursorTo(cursor, target, vp, 0.5).x,
      top: () => moveCursorTo(cursor, target, vp, 0.5).y,
    }, 0.4)
    .call(setStatus("executing", "executing · dom.click", "clicking e_4"), null, 1.4)
    .add(pulseCursor(cursor), 1.45)
    .to(target, { borderColor: "rgba(77,227,255,0.6)", duration: 0.2 }, 1.45)

    /* verification sweep — FAILS */
    .call(setStatus("executing", "verifying…", "verifying"), null, 2.0)
    .add(() => scan(scanEl, { fail: true, y: "55%" }), 2.1)
    .to(flash, { opacity: 1, duration: 0.25 }, 2.8)
    .to(flash, { opacity: 0, duration: 0.4 }, 3.1)
    .call(setStatus("failed", "verify failed — expected state not met", "verify failed"), null, 2.9)
    .call(() => brRetry.classList.add("on"), null, 3.4)

    /* retry — second pulse, verify PASSES */
    .call(setStatus("executing", "retry 1/2 · dom.click", "retrying 1/2"), null, 4.0)
    .add(pulseCursor(cursor), 4.05)
    .add(() => scan(scanEl, { fail: false, y: "55%" }), 4.6)
    .call(setStatus("verified", "verified — “Payment confirmed”", "verified ✓"), null, 5.4)
    .to(target, { borderColor: "rgba(61,255,162,0.6)", duration: 0.3 }, 5.4)
    .call(() => brRetry.classList.remove("on"), null, 5.6)

    /* ACT 2 — the abstain branch: cursor retreats, no candidate fires */
    .to({}, { duration: 1.0 })
    .call(setStatus("proposed", "goal: “comprar un vuelo”", "observing"), null, 7.0)
    .call(() => brAbstain.classList.add("on"), null, 7.6)
    .call(setStatus("abstained", "abstain — no candidate ≥ 0.65", "abstaining"), null, 7.9)
    .to(cursor, { left: "12%", top: "82%", duration: 1.0, ease: "power2.inOut" }, 7.9)
    .to(target, { borderColor: "", duration: 0.4 }, 8.4)
    .call(() => brAbstain.classList.remove("on"), null, 9.6);

  return tl;
}

/* ==================================================================
   SCROLL ORCHESTRATION
   ================================================================== */

const liveTweens = [];

/* image_scale_fade: hero viewport + bento cells grow into view */
gsap.utils.toArray(".hero-vp, .cell").forEach((el) => {
  liveTweens.push(gsap.fromTo(el,
    { scale: 0.9, opacity: 0.2 },
    {
      scale: 1, opacity: 1, duration: 0.9, ease: "power2.out",
      scrollTrigger: { trigger: el, start: "top 88%", toggleActions: "play none none reverse" },
    }));
});

/* card stacking: sticky position stacks the cards; ScrollTrigger only
   drives the depth cue — the covered scene sinks and dims as the next
   card slides over it. */
const scenes = gsap.utils.toArray(".scene");
scenes.forEach((scene, i) => {
  if (i === scenes.length - 1) return;
  liveTweens.push(gsap.to(scene, {
    scale: 0.94, opacity: 0.5, ease: "none",
    scrollTrigger: {
      trigger: scenes[i + 1], start: "top bottom", end: "top top+=84", scrub: true,
    },
  }));
});

/* scene timelines run only while their card is on screen */
function gate(id, make) {
  let tl = null;
  liveTweens.push(ScrollTrigger.create({
    trigger: "#" + id,
    start: "top 85%",
    end: "bottom top",
    onEnter: () => { if (!tl && !reduceMotion) tl = make(); tl && tl.play(); setStatus("runtime · " + id, true); },
    onEnterBack: () => { tl && tl.play(); },
    onLeave: () => { tl && tl.pause(); },
    onLeaveBack: () => { tl && tl.pause(); },
  }));
}

/* journal feed lights rows as the bento scrolls through */
liveTweens.push(ScrollTrigger.create({
  trigger: "#journalFeed", start: "top 80%",
  onEnter: () => {
    const rows = feed.querySelectorAll("li");
    gsap.to(rows, {
      opacity: 1, x: 0, stagger: 0.28, duration: 0.01,
      onStart() { rows.forEach((r) => r.classList.remove("lit")); },
    });
    rows.forEach((r, i) => gsap.delayedCall(0.3 + i * 0.28, () => r.classList.add("lit")));
    /* candidate bars sweep to their priors in sync */
    document.querySelectorAll(".cand").forEach((c, i) =>
      gsap.to(c.querySelector(".cand-bar"), {
        "--p": c._prior * 100, duration: 0.8, delay: 0.5 + i * 0.2, ease: "power2.out",
      }));
  },
}));

/* quotes carousel */
const quotes = [...document.querySelectorAll(".quote")];
let qi = 0;
function showQuote(n) {
  qi = (n + quotes.length) % quotes.length;
  quotes.forEach((q, i) => q.classList.toggle("active", i === qi));
}
document.getElementById("qPrev").addEventListener("click", () => showQuote(qi - 1));
document.getElementById("qNext").addEventListener("click", () => showQuote(qi + 1));
liveTweens.push(gsap.delayedCall(0, () => {})); /* keep array non-empty semantics */

/* accordion: first slice open by default */
document.querySelector(".slice").classList.add("open");
document.querySelectorAll(".slice").forEach((s) =>
  s.addEventListener("click", () => {
    document.querySelectorAll(".slice").forEach((x) => x.classList.remove("open"));
    s.classList.add("open");
  }));

/* boot */
if (reduceMotion) {
  /* end states only — no loops */
  document.querySelectorAll(".journal li").forEach((r) => r.classList.add("lit"));
  document.querySelectorAll(".cand").forEach((c) =>
    c.querySelector(".cand-bar").style.setProperty("--p", c._prior * 100));
  document.querySelectorAll(".pipeline li").forEach((s) => s.classList.add("on"));
  document.getElementById("heroHud").textContent = "verified — “Payment confirmed”";
  document.getElementById("verStatus").textContent = "verified — “Payment confirmed”";
  document.getElementById("verStatus").classList.add("verified");
  document.querySelector(".reticle .ret-tag").textContent = "e_40 · 0.81";
  document.getElementById("lockElB").classList.add("locked");
} else {
  gate("sceneLock", lockTimeline);
  gate("sceneFlight", flightTimeline);
  gate("sceneVerify", verifyTimeline);
  const heroTl = heroLoop();
  liveTweens.push(heroTl);
  ScrollTrigger.create({
    trigger: ".hero", start: "top bottom", end: "bottom top",
    onLeave: () => heroTl.pause(), onEnterBack: () => heroTl.play(),
  });
}
