// Svelte transitions that follow the motion tokens and honour `prefers-reduced-motion`
// (the CSS rule in app.css does not reach transitions run by Svelte).
import { cubicOut } from 'svelte/easing';
import { fade as svelteFade, fly as svelteFly, slide as svelteSlide, type SlideParams, type FlyParams, type FadeParams } from 'svelte/transition';

export function reducedMotion(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Expand/collapse (accordions, revealed sections). */
export function expand(node: Element, params: SlideParams = {}) {
  return svelteSlide(node, { duration: reducedMotion() ? 0 : 260, easing: cubicOut, ...params });
}

/** Content arriving from slightly below (list items, panels). */
export function rise(node: Element, params: FlyParams = {}) {
  return svelteFly(node, { y: 8, duration: reducedMotion() ? 0 : 220, easing: cubicOut, ...params });
}

export function fade(node: Element, params: FadeParams = {}) {
  return svelteFade(node, { duration: reducedMotion() ? 0 : 160, ...params });
}
