"""Clean shutdown before the house battery runs out (owner, 2026-11-23, instead of a UPS HAT).

The Pi runs from the house bank. If the bank is nearly empty, the battery BMS will eventually cut power, which
risks the history database and the drive. So when the bank's state of charge stays at or below `shutdown_soc`
for `hold_s` while not charging, AetherRV powers the Pi off cleanly through the root helper (`poweroff`).

  [power]
  auto_shutdown = true
  warn_soc = 8.0        # alert: the console will shut down soon
  shutdown_soc = 5.0    # power off at or below this...
  hold_s = 120          # ...held this long (rides out load spikes and a jumpy reading)

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
    def __init__(self, state: State, admin, cfg: dict | None = None) -> None:
        c = cfg or {}
        self.state, self.admin = state, admin
        self.warn_soc = float(c.get("warn_soc", 8.0))
        self.shutdown_soc = float(c.get("shutdown_soc", 5.0))
        self.hold_s = float(c.get("hold_s", 120.0))
        self.low_since: float | None = None
        self.done = False

    async def tick(self, now: float | None = None) -> None:
        now = time.time() if now is None else now
        soc, src = bank_soc(self.state)
        chg = charging(self.state)
        low = soc is not None and soc <= self.shutdown_soc and not chg
        if low:
            self.low_since = self.low_since or now
        else:
            self.low_since = None
        left = None if self.low_since is None else max(0.0, self.hold_s - (now - self.low_since))
        self.state.set("power.shutdown", {
            "soc": soc, "source": src, "charging": chg, "warn_soc": self.warn_soc, "shutdown_soc": self.shutdown_soc,
            "warning": soc is not None and soc <= self.warn_soc and not chg, "in_s": None if left is None else round(left),
        })
        if low and left == 0 and not self.done:
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
