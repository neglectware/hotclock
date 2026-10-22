# Notifications

The Notifications page sends email when something goes wrong or needs a person. For each kind of event, you decide how the message is sent.

Hotclock sends notifications about alerts (see [Alerts](alerts)). The page has three parts: the email channels, the rules, and a list of the messages that were sent.

## Set up an email channel {#email-channels}

A channel is one mail account that the box sends through. Fill in one card:

| Field | What it is |
|---|---|
| **Name** | A label for the list. |
| **Server**, **port**, and the security drop-down | The mail server. The security is `starttls` (usually port 587), `tls` (usually port 465) or `none`. |
| **Username** and **Password** | The login. The box stores the password in its secrets file (the `secrets_file` setting, readable only by the service). The password is never in the database, never in a backup of the database and never shown again. When a password is set, the box reads `set (leave empty to keep)`. Leave the box empty to keep the password. |
| **From** | The sender address. |
| **To** | One or more addresses, separated by commas. |
| **Enabled** | A channel that is not enabled sends nothing. |

To add a channel:

1. Fill in the empty card at the bottom of the list.
2. Press **Save channel**. The channel is saved, and a new empty card appears.
3. Press **Send test**. The box sends a message now.
4. Read the result under the buttons. It is the mail server's own reply, or its error.

The test shows a wrong password or a blocked port at once. The box keeps the result of the last test with the channel.

To remove a channel, press **Delete** and confirm.

When a rule names no channel, the rule uses **every enabled channel**.

## Decide how each alert is sent {#rules}

The card **When something happens** has one row for each alert code. The box saves every change at once.

| Column | What it does |
|---|---|
| **Event** | The alert code. See [Alerts](alerts) for the meaning of each code. |
| **On** | Clear this box to turn the event off completely. |
| **Severity** | `as raised` uses the alert's own severity. Or select `info`, `warning` or `critical` to force that severity in the message. |
| **Channels** | Hold Ctrl or Cmd to select more than one. When none is selected, the rule uses every enabled channel. |
| **Delivery** | `immediate`: one message at once. `hourly_digest`: collected and sent at the top of the next hour. `daily_digest`: collected and sent at 07:00 local time. `off`: nothing. |
| **Limit (min)** | For immediate delivery only. For each station, the box sends at most one message for the alert and one for its clearing in this many minutes (default 60). Anything more goes into the next hourly digest. |
| **Send on clear** | Also send a message when the problem goes away. |

### The shipped defaults

- **Serious problems** are sent **immediately**, and again when they clear. These are the problems that can take the station off the air: `silence`, `safe_mode`, `queue_starved`, `device_lost`, `engine_stall`, `media_store_failing`, `state_db_corrupt`, `library_db_unavailable`, `db_unavailable`, `disk_full`, `service_failed` and `stream_encoder_failed`.
- **Problems that need attention, but not at once,** are sent **immediately** with no message on clear: `item_quarantined`, `playlist_depleted`, `stream_no_listener` and `block_failed`.
- **Problems with single items, restarts and similar events** go in the **hourly digest**.
- **Slow-moving problems** go in the **daily digest**: `disk_low`, `backup_check_failed`, `import_failed`, `loudness_at_limit`, `loudness_frozen` and `media_verify_mismatch`.
- `decode_glitch` is **off**.

The section of each code on the [Alerts](alerts) page gives its default.

### Digests

A digest is one message. It has one line for each station and code, with the first time, the last time and how many times the alert was raised. A condition that comes and goes many times is still one line.

To send a digest before its time, press **Send the hourly digest now** or **Send the daily digest now**. The page says how many events went out, or that nothing is waiting.

An immediate message that the box cannot send is not lost. It goes in the next hourly digest.

### What a message looks like

The subject is `[<product> <station>] <SEVERITY> <code>`, or `[<product> <station>] cleared: <code>`. The body has the alert's text, the station, the code, the severity and the time.

## Recent messages {#recent-messages}

The list at the bottom shows the latest messages. Each line has the time, the kind, the code and the result. The kind is one of these:

- `immediate` or `immediate_clear`: a message sent at once, for the alert or for its clearing.
- a digest.
- `test`: a message from **Send test**.
- `suppressed`: a rate limit held the message back, and it went into the digest.

A failed send shows the mail server's error.

## When the library service itself fails

The library service sends the notifications. If that service stops, systemd runs a one-time command. The command sends a `service_failed` message through the same channels. See [Troubleshooting](troubleshooting).
