"""Clean shutdown before the house battery runs out (owner, 2026-11-23, instead of a UPS HAT).

The Pi runs from the house bank. If the bank is nearly empty, the battery BMS will eventually cut power, which
risks the history database and the drive. So when the bank's state of charge stays at or below `shutdown_soc`
for `hold_s` while not charging, AetherRV powers the Pi off cleanly through the root helper (`poweroff`).

  [power]
  auto_shutdown = true
  warn_soc = 8.0        # alert: the console will shut down soon
  shutdown_soc = 5.0    # power off at or below this...
  hold_s = 120          # ...held this long (rides out load spikes and a jumpy reading)
  heater_off_soc = 10.0 # owner 2026-11-23: if the Espar or the MCS7 is heating at or below this, turn BOTH off

Heat lockout (owner-approved automatic action, the one exception to "AetherRV acts only when a person asks"):
the heaters must never lose power while burning or in their after-run (owner: serious damage to the Espar and
the MCS7). So once the SoC has been at or below `heater_off_soc` for `hold_s` while not charging:
  * heat is LOCKED OUT (`power.heat_lockout`): both heaters' command paths refuse to start heat;
  * a heater found heating is turned off (Espar OFF; MCS7 furnace, electric, hot water, pre-heat off), again
    whenever one is found heating (at most once a minute), each a normal verified command ("auto:low-battery");
    each heater then runs its own after-run;
  * the lockout lifts when charging or the SoC is 2 % above the threshold.
Before the 5 % power-off, the heaters are turned off too and AetherRV waits (up to `heater_wait_s`) for both
to be idle, after-runs finished, so the Pi is the last thing to go and the heaters are cold before the BMS cuts.

SoC comes from the Victron shunt (victron.battery.soc) while it's fresh, else the mean of the batteries read
over Bluetooth. Charging (bank current above +1 A) or a SoC back above the threshold cancels the countdown.
No SoC reading at all never triggers a shutdown. A Pi 5 stays off afterwards until its power is cycled (e.g.
the BMS cuts and later restores it) or its power button is pressed.
"""

from __future__ import annotations

import asyncio
import logging
import time

from .state import State

log = logging.getLogger(__name__)

FRESH_S = 120.0        # a SoC or current older than this is ignored
CHARGING_A = 1.0


def bank_soc(s: State) -> tuple[float | None, str | None]:
    soc, age = s.get("victron.battery.soc"), s.age("victron.battery.soc")
    if soc is not None and age is not None and age < FRESH_S:
        return float(soc), "shunt"
    vals = [float(r["soc"]) for r in s.get("batteries.ble") or [] if r.get("ok") and r.get("soc") is not None]
    return (round(sum(vals) / len(vals), 1), "batteries") if vals else (None, None)


def charging(s: State) -> bool:
    cur, age = s.get("victron.battery.current"), s.age("victron.battery.current")
    if cur is not None and age is not None and age < FRESH_S:
        return float(cur) > CHARGING_A
    ble = s.get("batteries.ble.current")
    return ble is not None and float(ble) > CHARGING_A


class LowBatteryShutdown:
    def __init__(self, state: State, admin, cfg: dict | None = None, controls: dict | None = None) -> None:
        c = cfg or {}
        self.state, self.admin, self.controls = state, admin, controls or {}
        self.heater_off_soc = float(c.get("heater_off_soc", 10.0))
        self.heater_wait_s = float(c.get("heater_wait_s", 600.0))
        self.heat_low_since: float | None = None
        self.lockout = False
        self.last_off: float | None = None
        self.off_wait_since: float | None = None
        self.warn_soc = float(c.get("warn_soc", 8.0))
        self.shutdown_soc = float(c.get("shutdown_soc", 5.0))
        self.hold_s = float(c.get("hold_s", 120.0))
        self.low_since: float | None = None
        self.done = False

    def heating(self) -> dict:
        s = self.state
        return {"espar": bool(s.get("climate.espar.ctl.active")) and s.get("climate.espar.ctl.mode") == "heat"
                         or s.get("climate.espar.state") in ("heating", "starting"),
                "mcs7": s.get("climate.rixens.state") == "heating" or s.get("climate.rixens.electric") == "active"}

    def busy(self) -> bool:
        """A heater is burning or still in its after-run (power must stay on)."""
        s = self.state
        return (any(self.heating().values()) or s.get("climate.espar.state") == "cooldown"
                or s.get("climate.rixens.heater_state_code") in (10, 12, 13, 15, 20, 30, 31))

    async def heaters_off(self, soc: float) -> list[str]:
        """Espar OFF, then the MCS7's heat sources and modes that are on. Returns what was asked and the result."""
        s, done = self.state, []
        why = f"house battery at {soc:.0f}%"
        esp = self.controls.get("espar")
        if esp is not None and s.get("climate.espar.ctl.active") and s.get("climate.espar.ctl.mode") not in (None, "off"):
            r = await esp.command("set", {"mode": "off"}, client="auto:low-battery")
            done.append(f"Espar off: {r.get('detail')}")
        mcs = self.controls.get("rixens")
        if mcs is not None and s.get("climate.rixens.live"):
            for cmd, on in (("furnace", "furnace" in (s.get("climate.rixens.sources") or [])),
                            ("electric", "electric" in (s.get("climate.rixens.sources") or [])),
                            ("hot_water", bool(s.get("climate.rixens.hot_water"))),
                            ("preheat", bool(s.get("climate.rixens.preheat")))):
                if on:
                    r = await mcs.command(cmd, False, client="auto:low-battery")
                    done.append(f"MCS7 {cmd} off: {r.get('detail')}")
        log.warning("%s and a heater was heating: %s", why, "; ".join(done) or "nothing to switch off")
        return done

    async def tick(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        soc, src = bank_soc(self.state)
        chg = charging(self.state)
        # heat lockout at/below heater_off_soc (held hold_s, not charging); lifted by charging or +2 %
        heat_low = soc is not None and soc <= self.heater_off_soc and not chg
        if heat_low:
            self.heat_low_since = now if self.heat_low_since is None else self.heat_low_since
            if now - self.heat_low_since >= self.hold_s and not self.lockout:
                self.lockout = True
                log.warning("house battery at %.1f%%: heat locked out", soc)
        else:
            self.heat_low_since = None
            if self.lockout and (chg or (soc is not None and soc > self.heater_off_soc + 2)):
                self.lockout = False
                log.warning("house battery recovered (%s%%, charging=%s): heat lockout lifted", soc, chg)
                self.state.set("power.heaters_off", None)
        self.state.set("power.heat_lockout", self.lockout)
        if self.lockout and any(self.heating().values()) and (self.last_off is None or now - self.last_off >= 60):
            self.last_off = now
            done = await self.heaters_off(soc)
            self.state.set("power.heaters_off", {"t": round(now), "soc": soc, "done": done})
        low = soc is not None and soc <= self.shutdown_soc and not chg
        if low:
            self.low_since = now if self.low_since is None else self.low_since
        else:
            self.low_since = None
        left = None if self.low_since is None else max(0.0, self.hold_s - (now - self.low_since))
        self.state.set("power.shutdown", {
            "soc": soc, "source": src, "charging": chg, "warn_soc": self.warn_soc, "shutdown_soc": self.shutdown_soc,
            "warning": soc is not None and soc <= self.warn_soc and not chg, "in_s": None if left is None else round(left),
        })
        if low and left == 0 and not self.done:
            # the heaters first: off, then wait until both are idle (after-runs done) before the Pi goes
            if self.busy():
                if self.off_wait_since is None:
                    self.off_wait_since = now
                    await self.heaters_off(soc)
                if now - self.off_wait_since < self.heater_wait_s:
                    self.state.set("power.shutdown", {**self.state.get("power.shutdown"), "waiting_for_heaters": True})
                    return
                log.error("heaters still busy after %.0f s; powering off anyway", self.heater_wait_s)
            self.done = True
            log.warning("house battery at %.1f%% (%s) for %.0f s and not charging: powering off cleanly", soc, src, self.hold_s)
            try:
                r = await self.admin.call("poweroff")
                log.warning("poweroff requested: %s", r)
            except Exception as e:                    # noqa: BLE001 — report; try again on a later tick
                log.error("poweroff failed: %s", e)
                self.done = False

    async def run(self, poll_s: float = 10.0) -> None:
        while True:
            await self.tick()
            await asyncio.sleep(poll_s)
