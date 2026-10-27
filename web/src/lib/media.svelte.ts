/** Reactive media query, e.g. `const narrow = mediaQuery('(max-width: 899px)')`. */
export function mediaQuery(query: string): { readonly matches: boolean } {
  const state = $state({ matches: false });
  $effect(() => {
    const list = matchMedia(query);
    state.matches = list.matches;
    const update = () => (state.matches = list.matches);
    list.addEventListener('change', update);
    return () => list.removeEventListener('change', update);
  });
  return state;
}
