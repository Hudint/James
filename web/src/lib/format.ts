/** Formats milliseconds as m:ss or h:mm:ss. */
export function formatDuration(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '–';
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

export function trackLabel(track: { title: string; artist: string | null }): string {
  // Many titles already name the artist ("Daft Punk - Get Lucky"): no need to repeat it.
  if (!track.artist || track.title.toLowerCase().startsWith(track.artist.toLowerCase())) return track.title;
  return `${track.artist} – ${track.title}`;
}
