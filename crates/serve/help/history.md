# History

The History page is the as-run log. It lists everything the station played, newest first. For each item it shows how the item ended and how close it started to its planned time.

Select the station with the buttons at the top. The table shows the last 200 plays.

**Export CSV** downloads the log as `as-run.csv`. A station can keep this file as its record of what went to air.

| Column | Meaning |
|---|---|
| **Started** | When the item began to sound, in your browser's time zone. |
| **Title**, **Artist**, **Category** | From the item, as it was at that time. |
| **Origin** | Why the item played: `rotation` (the playlist), `liner`, `id`, `block` (a timed block put it there), `manual` (a person queued it), or `restart` (a row that marks a restart). |
| **Ended by** | How the item ended. `on air` means that it is playing now. See below. |
| **Played** | How long the item actually played. |
| **Deviation** | How many seconds after its planned start time the item really started. A small number is normal. A large number shows that the plan had to change. |

## How an item ended

| Value | Meaning |
|---|---|
| `end` | The item ended normally. This includes an ordinary song change. |
| `next` | A person pressed **Next**. |
| `fade` | A person pressed **Fade to next**. |
| `cut` | A person pressed **Cut**. |
| `stop` | The station was stopped, by **Stop** or by a stop block. |
| `error` | The item failed while it played. It could not be decoded, or it ended early. |
| `restart` | This row is not an item. It records the time the station was down and the seconds without sound. |
| `crash` | The item was playing when the process stopped without warning. |

Some rows carry flags. The table and the CSV do not show them, but the API returns them. For example:

- `rule_relaxed`: a no-repeat rule had to give way because nothing else qualified.
- `hard_time`: a timed block cut in.
- `clock_jump_ms=N`: the clock moved during the row.

The log is in the station's own database. It survives a restart, and the nightly snapshot includes it. See [Upgrading, backup and restore](upgrading-backup).
