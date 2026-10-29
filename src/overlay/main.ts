/**
 * Overlay de sélection : l'écran figé, assombri, et une zone découpée en pleine lumière.
 * Tout le dessin passe par deux canvas ; aucun reflow pendant le glisser.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { applyI18n, initLang } from "../lib/i18n";
import { fetchShot } from "../lib/ipc";

const shotCanvas = document.querySelector<HTMLCanvasElement>("#shot")!;
const ui = document.querySelector<HTMLCanvasElement>("#ui")!;
const badge = document.querySelector<HTMLDivElement>("#badge")!;
const hint = document.querySelector<HTMLDivElement>("#hint")!;
const shotCtx = shotCanvas.getContext("2d", { alpha: false })!;
const ctx = ui.getContext("2d")!;

const DIM = "rgba(8, 9, 14, 0.5)";
const ACCENT = "#ff7a59";
const LOUPE_SIZE = 116;
const LOUPE_ZOOM = 8;

type Rect = { x: number; y: number; w: number; h: number };

let image: ImageBitmap | null = null;
let ready = false;
let dragging = false;
let committing = false;
let origin = { x: 0, y: 0 };
let pointer: { x: number; y: number } | null = null;
let selection: Rect | null = null;
let frame = 0;

/** Rapport pixels image / pixels CSS (≈ devicePixelRatio de l'écran capturé). */
function ratio(): number {
  return image ? image.width / window.innerWidth : window.devicePixelRatio;
}

function resizeUi(): void {
  const dpr = window.devicePixelRatio;
  ui.width = Math.round(window.innerWidth * dpr);
  ui.height = Math.round(window.innerHeight * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  invalidate();
}

function normalize(a: { x: number; y: number }, b: { x: number; y: number }): Rect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.abs(a.x - b.x),
    h: Math.abs(a.y - b.y),
  };
}

function invalidate(): void {
  if (!frame) frame = requestAnimationFrame(draw);
}

function draw(): void {
  frame = 0;
  const W = window.innerWidth;
  const H = window.innerHeight;
  ctx.clearRect(0, 0, W, H);

  // Voile + découpe (règle evenodd : la sélection reste en pleine lumière).
  ctx.beginPath();
  ctx.rect(0, 0, W, H);
  if (selection && selection.w > 0 && selection.h > 0) {
    ctx.rect(selection.x, selection.y, selection.w, selection.h);
  }
  ctx.fillStyle = DIM;
  ctx.fill("evenodd");

  if (selection && selection.w > 0 && selection.h > 0) {
    drawSelection(selection);
  } else if (pointer) {
    drawGuides(pointer, W, H);
  }

  if (pointer && image) drawLoupe(pointer, W, H);
  updateBadge();
}

function drawSelection(r: Rect): void {
  const x = Math.round(r.x) + 0.5;
  const y = Math.round(r.y) + 0.5;
  const w = Math.round(r.w);
  const h = Math.round(r.h);
  ctx.lineWidth = 1;
  ctx.strokeStyle = "rgba(0, 0, 0, 0.45)";
  ctx.strokeRect(x - 1, y - 1, w + 2, h + 2);
  ctx.strokeStyle = "rgba(255, 255, 255, 0.92)";
  ctx.strokeRect(x, y, w, h);

  // Coins en équerre, accent : on voit où l'on tire.
  const len = Math.min(14, w / 2, h / 2);
  ctx.strokeStyle = ACCENT;
  ctx.lineWidth = 3;
  ctx.lineCap = "round";
  ctx.beginPath();
  for (const [cx, cy, dx, dy] of [
    [x, y, 1, 1],
    [x + w, y, -1, 1],
    [x, y + h, 1, -1],
    [x + w, y + h, -1, -1],
  ]) {
    ctx.moveTo(cx, cy + dy * len);
    ctx.lineTo(cx, cy);
    ctx.lineTo(cx + dx * len, cy);
  }
  ctx.stroke();
}

function drawGuides(p: { x: number; y: number }, W: number, H: number): void {
  ctx.save();
  ctx.strokeStyle = "rgba(255, 255, 255, 0.28)";
  ctx.lineWidth = 1;
  ctx.setLineDash([4, 4]);
  ctx.beginPath();
  ctx.moveTo(0, Math.round(p.y) + 0.5);
  ctx.lineTo(W, Math.round(p.y) + 0.5);
  ctx.moveTo(Math.round(p.x) + 0.5, 0);
  ctx.lineTo(Math.round(p.x) + 0.5, H);
  ctx.stroke();
  ctx.restore();
}

/** Loupe ×8 au pixel près, placée à l'opposé des bords. */
function drawLoupe(p: { x: number; y: number }, W: number, H: number): void {
  const k = ratio();
  const size = LOUPE_SIZE;
  const offset = 22;
  let lx = p.x + offset;
  let ly = p.y + offset;
  if (lx + size > W - 8) lx = p.x - offset - size;
  if (ly + size + 26 > H - 8) ly = p.y - offset - size - 26;

  const cells = size / LOUPE_ZOOM;
  const sx = Math.round(p.x * k - cells / 2);
  const sy = Math.round(p.y * k - cells / 2);

  ctx.save();
  ctx.beginPath();
  ctx.roundRect(lx, ly, size, size, 12);
  ctx.clip();
  ctx.imageSmoothingEnabled = false;
  ctx.fillStyle = "#101116";
  ctx.fillRect(lx, ly, size, size);
  ctx.drawImage(shotCanvas, sx, sy, cells, cells, lx, ly, size, size);

  ctx.strokeStyle = "rgba(255, 255, 255, 0.06)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let i = 1; i < cells; i++) {
    const o = Math.round(i * LOUPE_ZOOM) + 0.5;
    ctx.moveTo(lx + o, ly);
    ctx.lineTo(lx + o, ly + size);
    ctx.moveTo(lx, ly + o);
    ctx.lineTo(lx + size, ly + o);
  }
  ctx.stroke();
  const c = Math.floor(cells / 2) * LOUPE_ZOOM;
  ctx.strokeStyle = ACCENT;
  ctx.lineWidth = 2;
  ctx.strokeRect(lx + c, ly + c, LOUPE_ZOOM, LOUPE_ZOOM);
  ctx.restore();

  ctx.strokeStyle = "rgba(255, 255, 255, 0.2)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.roundRect(lx + 0.5, ly + 0.5, size - 1, size - 1, 12);
  ctx.stroke();

  const label = `${Math.round(p.x * k)}, ${Math.round(p.y * k)}`;
  ctx.font = '500 11px "Cascadia Mono", "SF Mono", Consolas, monospace';
  const tw = ctx.measureText(label).width + 14;
  ctx.fillStyle = "rgba(20, 21, 28, 0.92)";
  ctx.beginPath();
  ctx.roundRect(lx + (size - tw) / 2, ly + size + 6, tw, 20, 6);
  ctx.fill();
  ctx.fillStyle = "rgba(240, 241, 245, 0.9)";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(label, lx + size / 2, ly + size + 16.5);
}

function updateBadge(): void {
  if (!selection || selection.w < 1 || selection.h < 1) {
    badge.hidden = true;
    return;
  }
  const k = ratio();
  badge.textContent = `${Math.round(selection.w * k)} × ${Math.round(selection.h * k)}`;
  badge.hidden = false;
  const bw = badge.offsetWidth;
  let bx = selection.x;
  let by = selection.y - 30;
  if (by < 8) by = selection.y + 8;
  if (bx + bw > window.innerWidth - 8) bx = window.innerWidth - 8 - bw;
  badge.style.transform = `translate(${Math.round(bx)}px, ${Math.round(by)}px)`;
}

function reset(): void {
  dragging = false;
  committing = false;
  selection = null;
  pointer = null;
  badge.hidden = true;
  hint.classList.remove("is-hidden");
  ctx.clearRect(0, 0, ui.width, ui.height);
}

async function commit(r: Rect): Promise<void> {
  if (committing || !image) return;
  committing = true;
  const k = ratio();
  const x = Math.max(0, Math.round(r.x * k));
  const y = Math.max(0, Math.round(r.y * k));
  const width = Math.min(image.width - x, Math.round(r.w * k));
  const height = Math.min(image.height - y, Math.round(r.h * k));
  try {
    await invoke("commit_selection", { x, y, width, height });
  } finally {
    ready = false;
    reset();
  }
}

async function cancel(): Promise<void> {
  ready = false;
  reset();
  await invoke("cancel_capture");
}

ui.addEventListener("pointerdown", (e) => {
  if (!ready || e.button !== 0) return;
  ui.setPointerCapture(e.pointerId);
  dragging = true;
  origin = { x: e.clientX, y: e.clientY };
  selection = { x: e.clientX, y: e.clientY, w: 0, h: 0 };
  hint.classList.add("is-hidden");
  invalidate();
});

ui.addEventListener("pointermove", (e) => {
  pointer = { x: e.clientX, y: e.clientY };
  if (dragging) selection = normalize(origin, pointer);
  invalidate();
});

ui.addEventListener("pointerup", (e) => {
  if (!dragging) return;
  dragging = false;
  ui.releasePointerCapture(e.pointerId);
  const r = selection;
  if (r && r.w >= 4 && r.h >= 4) {
    void commit(r);
  } else {
    selection = null;
    hint.classList.remove("is-hidden");
    invalidate();
  }
});

ui.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  void cancel();
});

window.addEventListener("keydown", (e) => {
  if (!ready) return;
  if (e.key === "Escape") {
    e.preventDefault();
    void cancel();
  } else if (e.key === " " || e.key === "Enter") {
    e.preventDefault();
    void commit({ x: 0, y: 0, w: window.innerWidth, h: window.innerHeight });
  }
});

window.addEventListener("resize", resizeUi);

// Perte de focus (Alt+Tab…) : on abandonne proprement plutôt que de laisser un voile orphelin.
window.addEventListener("blur", () => {
  if (ready && !committing) void cancel();
});

async function onCapture(): Promise<void> {
  reset();
  image?.close();
  image = await fetchShot("get_capture");
  shotCanvas.width = image.width;
  shotCanvas.height = image.height;
  shotCtx.drawImage(image, 0, 0);
  resizeUi();
  draw();
  document.body.classList.remove("is-entering");
  void document.body.offsetWidth;
  document.body.classList.add("is-entering");
  // Deux frames : l'image est peinte avant que la fenêtre n'apparaisse (aucun flash).
  // Une fenêtre encore masquée peut ne recevoir aucune frame : au-delà de 120 ms,
  // on s'affiche quand même plutôt que de rester invisible indéfiniment.
  await Promise.race([
    new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
    new Promise((r) => setTimeout(r, 120)),
  ]);
  ready = true;
  await invoke("overlay_ready");
}

async function main(): Promise<void> {
  await initLang();
  applyI18n();
  resizeUi();
  await listen("overlay:capture", () => {
    onCapture().catch((err) => {
      console.error(err);
      void cancel();
    });
  });
}

void main();
