// Compact relative timestamps for list rows ("2 min ago", "3 h ago").
// Beyond a week the calendar date is more useful than a large count.

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;

/** How long a run took, compact: "8 s", "2 min 30 s", "5 min". Jobs are
 *  minutes-scale at worst (the reaper kills stuck ones), so minutes cap it. */
export function durationBetween(startIso: string, endIso: string): string {
  const seconds = Math.max(
    0,
    Math.round((new Date(endIso).getTime() - new Date(startIso).getTime()) / 1000),
  );
  if (seconds < MINUTE / 1000) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return rest === 0 ? `${minutes} min` : `${minutes} min ${rest} s`;
}

export function timeAgo(iso: string, now: Date = new Date()): string {
  // Clamp at zero: client/server clock skew must never render "in 30 s".
  const elapsed = Math.max(0, now.getTime() - new Date(iso).getTime());
  if (elapsed < MINUTE) return "just now";
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)} min ago`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)} h ago`;
  if (elapsed < WEEK) return `${Math.floor(elapsed / DAY)} d ago`;
  return iso.slice(0, 10);
}
