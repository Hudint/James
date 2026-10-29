/** Reactive media query, e.g. `const narrow = mediaQuery('(max-width: 899px)')`. */
export function mediaQuery(query: string): { readonly matches: boolean } {
  // The right value from the start (no layout flash before the effect runs).
  const state = $state({ matches: typeof window !== 'undefined' && window.matchMedia(query).matches });
  $effect(() => {
    const list = matchMedia(query);
    state.matches = list.matches;
    const update = () => (state.matches = list.matches);
    list.addEventListener('change', update);
    return () => list.removeEventListener('change', update);
  });
  return state;
}
