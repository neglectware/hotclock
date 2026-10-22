# System

The System page shows the health of the library service and of every station. On it you set each station's output level and crossfade style, and you see the items that are in quarantine.

## Library service {#library-service}

The first card shows the status of the library service: `ok` in green, anything else in red. A line of facts follows:

- how many items the library holds;
- the library's **revision**, a number that goes up with every change (stations use it to know when to reload);
- how long the service has been up;
- the free disk space for the media;
- how many notification messages the service has sent.

## Stations {#stations}

There is one card for each station.

- **The chip** is the station's health: `ok`, `warning` or `critical`. Beside it are the station's state and its address. A station that does not answer shows `not answering`. `critical` means that an alert of critical severity is active, or that the station is off the air. See [Alerts](alerts).
- **Processing chain** opens the station's chain on the **Processing** page. That page shows one station at a time, with a tab for each station. See [The processing chain](processing#the-panel).
- **Output trim (dB)** is one gain at the very end of the signal path. See [Output and level](output-and-level#output-trim).
- **Crossfade style** is the station's default crossfade style. See [Crossfades](crossfades#choosing-a-style).
- **All settings of this station** opens the [Settings](settings) page for the station.

To set the output trim:

1. Type a value from -40 to +6 in **Output trim (dB)**. 0 is unity gain.
2. Press **Set**. The text beside the button shows the new value.

## Quarantine {#quarantine}

The last card lists every item in quarantine. The number in the title is how many there are. Each row has the reason, a link to the item and a **Release** button.

A quarantined item is never played. It stays in the library with its history. See [Failures and recovery](failures-and-recovery#quarantine) for how an item gets into quarantine.

To release an item, press **Release** on its row. This does three things:

- It clears the quarantine in the library.
- It clears the quarantine on every station of this box.
- It sets the item's error count back to 0.

If a station does not answer, a toast tells you.

## Health from outside

Other programs can read everything on this page:

```
curl -s http://<box>:8300/health      # the library service
curl -s http://<box>:8301/health      # a station: 200 for ok and warning, 503 for critical
curl -s http://<box>:8301/metrics     # counters and gauges in the Prometheus text format
```

A monitoring tool can poll the `/health` addresses.
