// Which web interface is loaded: version, commit and build time (set by vite.config.ts).
export const build = __JAMES_BUILD__;

/** Short commit id (empty when unknown). */
export const shortRevision = build.revision.slice(0, 7) + (build.revision.endsWith('-dirty') ? '-dirty' : '');

export function builtAt(locale: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(build.time));
}

/** Logs the build to the developer console, once per page load. */
export function announceBuild() {
  console.info(
    `%cJames%c ${build.version} · built ${build.time}${build.revision ? ` · commit ${build.revision}` : ''}`,
    'background:#f26b1d;color:#0e1a33;font-weight:700;padding:1px 6px;border-radius:3px',
    'color:inherit',
  );
}
