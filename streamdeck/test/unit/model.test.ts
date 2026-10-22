import { describe, expect, it } from "vitest";
import { StationModel } from "../../src/core/model.js";
import { BOX0, ev, FakeClock, iso, liveModel, nowJson, snapshot, timing } from "./helpers.js";

describe("StationModel", () => {
  it("reads the snapshot: what plays, what is next, the playlist", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    expect(m.ready).toBe(true);
    expect(m.now?.title).toBe("From Silence");
    expect(m.next?.title).toBe("There Is Love");
    expect(m.activePlaylist).toEqual({ id: 1, name: "Pre-show" });
    expect(m.toSegueMs()).toBe(180_000);
    expect(m.elapsedMs()).toBe(0);
  });

  it("counts down locally between events, on its own clock", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    clock.advance(12_340);
    expect(m.toSegueMs()).toBe(180_000 - 12_340);
    expect(m.elapsedMs()).toBe(12_340);
  });

  it("takes a timing event as the correction", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    clock.advance(10_000);
    // The box says 169.2 s remain: 0.8 s less than the local count.
    m.apply(timing(10_800, 169_200, 189_200, BOX0 + 10_800));
    expect(m.toSegueMs()).toBe(169_200);
    clock.advance(500);
    expect(m.toSegueMs()).toBe(168_700);
  });

  it("takes a timing event as of the moment the box sent it, not the moment it was read", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    // A quick event, then one that sat 300 ms in a busy process before it was read.
    clock.advance(1000);
    m.apply(timing(1000, 179_000, 199_000, BOX0 + 1000));
    clock.advance(1000);
    m.apply(timing(2000, 178_000, 198_000, BOX0 + 2000));
    clock.advance(1000 + 300);
    m.apply(timing(3000, 177_000, 197_000, BOX0 + 3000));
    // Right after reading it: 177 s as of 300 ms ago, so 176.7 s now.
    expect(m.toSegueMs()).toBe(176_700);
  });

  it("ignores a timing event for another entry", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(timing(1000, 5000, 6000, BOX0, 99));
    expect(m.toSegueMs()).toBe(180_000);
  });

  it("starts a new countdown at a now event", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    clock.advance(179_000);
    const startedBox = BOX0 + 180_000;
    m.apply(ev("now", nowJson({ entry_id: 8, item_id: 43, title: "There Is Love", art_url: null }, startedBox, 150_000, 170_000), startedBox));
    expect(m.now?.title).toBe("There Is Love");
    expect(m.toSegueMs()).toBe(150_000);
  });

  it("counts to the end instead of the segue inside a gapless run", () => {
    const clock = new FakeClock();
    const m = liveModel(clock, { now: nowJson({ continuity_next: true }) });
    expect(m.remainingMs()).toEqual({ ms: 200_000, to: "end" });
    const m2 = liveModel(clock);
    expect(m2.remainingMs()).toEqual({ ms: 180_000, to: "segue" });
  });

  it("learns the box's clock from the messages", () => {
    const clock = new FakeClock();
    const m = new StationModel(clock.now);
    m.setOpen();
    m.apply(snapshot({}, BOX0 + 5000));
    expect(m.boxNow()).toBe(BOX0 + 5000);
    clock.advance(1000);
    expect(m.boxNow()).toBe(BOX0 + 6000);
  });

  it("clears the playing song when the station stops", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("state", { state: "stopped", mode: "auto", hold: false, safe_mode: false, active_playlist: null }));
    expect(m.now).toBeNull();
    expect(m.remainingMs()).toBeNull();
  });

  it("takes the next song from the queue at once, and shows no playing song in a snapshot of a stopped station", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("queue", { entries: [{ kind: "track", item_id: 77, title: "Reshuffled", artist: "X", art_url: null, planned_start: iso(BOX0 + 120_000) }] }));
    expect(m.next?.title).toBe("Reshuffled");
    expect(m.next?.itemId).toBe(77);
    expect(m.next?.startsMs).toBe(BOX0 + 120_000);
    const stopped = new StationModel(clock.now);
    stopped.setOpen();
    stopped.apply(snapshot({ state: "stopped" }));
    expect(stopped.now).toBeNull();
  });

  it("does not count a halted or stopped program as running", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(ev("program", { running: true, program: { run_id: 1, playlist_id: 3, state: "halted" } }));
    expect(m.program?.running).toBe(false);
    m.apply(ev("program", { running: true, program: { run_id: 1, playlist_id: 3, state: "stopped" } }));
    expect(m.program?.running).toBe(false);
    m.apply(ev("program", { running: true, program: { run_id: 1, playlist_id: 3, state: "running" } }));
    expect(m.program?.running).toBe(true);
  });

  it("keeps nothing after a disconnect: no stale now-playing", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.applyProgramState({ running: true, program: { run_id: 1, playlist_id: 3, state: "running" } });
    m.reset();
    expect(m.online).toBe(false);
    expect(m.ready).toBe(false);
    expect(m.now).toBeNull();
    expect(m.next).toBeNull();
    expect(m.program).toBeNull();
    expect(m.alerts).toEqual([]);
    expect(m.remainingMs()).toBeNull();
  });

  it("tracks alerts and the worst severity", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    expect(m.status()).toBe("ok");
    m.apply(ev("alert", { id: 1, code: "disk_low", severity: "warning", message: "x" }));
    expect(m.status()).toBe("warning");
    m.apply(ev("alert", { id: 2, code: "no_audio", severity: "critical", message: "y" }));
    expect(m.status()).toBe("critical");
    m.apply(ev("alert_cleared", { id: 2 }));
    m.apply(ev("alert_cleared", { id: 1 }));
    expect(m.status()).toBe("ok");
  });

  it("reads the program: next block, expected start, waiting, back-time", () => {
    const clock = new FakeClock();
    const m = liveModel(clock);
    m.apply(
      ev("program", {
        running: true,
        program: { run_id: 4, playlist_id: 3, playlist: "Eve", state: "waiting", waiting: { block_id: 9, name: "Hold for host", kind: "operator" } },
        next_block: { block_id: 11, name: "Top of hour ID", type: "insert", badge: "ID", in_ms: 45_000 },
        expected: { playlist_id: 3, playlist: "Eve", at: iso(BOX0), in_ms: 600_000, armed: true, label: "expected 18:45" },
        back_time: { state: "planned", label: "END +3 s", delta_s: 3, target: iso(BOX0 + 900_000) },
        blocks: [{ block_id: 11, status: "pending" }, { block_id: 12, status: "fired" }],
        time_zone: "America/New_York",
      }),
    );
    const p = m.program!;
    expect(p.state).toBe("waiting");
    expect(p.waiting).toEqual({ blockId: 9, name: "Hold for host", kind: "operator" });
    expect(p.nextBlock?.dueLocal).toBe(clock.now() + 45_000);
    expect(p.expected?.armed).toBe(true);
    expect(p.backTime?.label).toBe("END +3 s");
    expect(p.blocks.find((b) => b.blockId === 12)?.status).toBe("fired");
    expect(p.timeZone).toBe("America/New_York");
  });
});
