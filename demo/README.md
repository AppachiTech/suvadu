# Suvadu 0.5.0 demos

Three recordings show the actual local 0.5.0 interface using entirely fictional data:

| Asset | Story | Duration |
| --- | --- | --- |
| `hero.gif` | Ctrl+R → search docker logs → return command to prompt | 22 s |
| `suvadu-home.gif` | Browse Home → find failed commands → open search → return | 20 s |
| `suvadu-ai.gif` | AI session prompt → failed test → successful retry → response | 28 s |

The command selected in Search is not executed. AI prompts, responses, statuses,
paths, times, and command counts are staged fixtures, not a live agent run.
No personal history or configuration was copied into the recording environment.

GIFs are 960 pixels wide at 8 fps for the GitHub README. The website uses MP4,
WebM, and WebP posters under `suvadu-web/public/demo/v0.5.0/`, reusing the same
three recordings on the homepage, relevant CLI guides, and comparison article.
Native video controls let visitors pause; documentation recordings do not autoplay.

The capture used a real zsh PTY with the actual binary, an isolated HOME, and a
macOS sandbox denying personal files and network access. ANSI output was rendered
with pyte/Pillow, then encoded with FFmpeg; these are not VHS-rendered videos.
The old local `.tape` files are legacy scripts and are not the source of these
exports. Never run those scripts against personal history for public demos.

Capture scripts, authored fixture scripts, ANSI recordings, QA frames, and the
binary checksum are retained in the local admin workspace under
`suv_admin/video_demo_2026-09-27/final/`. No application UI code was modified.
