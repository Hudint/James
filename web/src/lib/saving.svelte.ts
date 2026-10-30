// Saving tracks of other sources (YouTube, …) to the library, shared by all places offering it.
import { api, ApiError } from './api';
import { t } from './i18n/index.svelte';
import { can, dismissToast, reportError, toast } from './session.svelte';
import type { LibraryTrack, Track } from './types';

/** Inputs being saved right now (a second click on the same one does nothing), and those saved
 * in this session (shown as saved). */
export const saving = $state<{ inputs: string[]; done: string[]; version: number }>({ inputs: [], done: [], version: 0 });

/** How often and how long a save going on in the background is asked about. */
const POLL_MS = 4000;
const POLL_FOR_MS = 20 * 60_000;

/** Waits for a background save; the saved track, or an error. */
async function finished(job: string): Promise<LibraryTrack> {
  const end = Date.now() + POLL_FOR_MS;
  while (Date.now() < end) {
    await new Promise((resolve) => setTimeout(resolve, POLL_MS));
    // Network hiccups are retried; an unknown job (expired, server restarted) is not.
    const status = await api.saveStatus(job).catch((e) => {
      if (e instanceof ApiError && e.status === 404) throw new Error(t('library.saveLost'));
      return null;
    });
    if (status?.state === 'done') return status.track;
    if (status?.state === 'failed') throw new Error(status.error);
  }
  throw new Error(t('library.saveTimeout'));
}

/** A link or prefixed input (`yt:`, `sc:`, …) as typed; other text searches YouTube. */
export function toSaveInput(value: string): string {
  const v = value.trim();
  return /^(https?:\/\/|(yt|sc|search):)/i.test(v) ? v : `yt:${v}`;
}

/** Whether `value` is a search (saving takes its first result). */
export const isSearch = (value: string) => toSaveInput(value).startsWith('yt:') && !/^yt:/i.test(value.trim());

export const canSave = () => can('library.download');

/** Whether a track can be saved: from yt-dlp (YouTube, SoundCloud, …), not live. */
export const savable = (track: Track | null | undefined) => !!track && track.source === 'ytdlp' && !track.isStream;

/** Saves what `input` (a link or search) is; `null` if it failed (the error is shown). */
export async function saveToLibrary(input: string): Promise<LibraryTrack | null> {
  const value = input.trim();
  if (!value || saving.inputs.includes(value)) return null;
  saving.inputs.push(value);
  const progress = toast(t('library.saving'), 'info', { sticky: true });
  try {
    const answer = await api.saveToLibrary(value);
    // Longer saves go on in the server: their outcome is asked for until it is there.
    const track = 'pending' in answer ? await finished(answer.job) : answer;
    saving.done.push(value);
    saving.version++;
    toast(t('library.saved', { title: track.title }), 'success');
    return track;
  } catch (e) {
    reportError(e);
    return null;
  } finally {
    dismissToast(progress);
    saving.inputs = saving.inputs.filter((i) => i !== value);
  }
}
