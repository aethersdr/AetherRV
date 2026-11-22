"""The single choke point for anything that changes a device.

Every transmit (CAN frame, MQTT write, ...) goes through `TxGate.check()`. Writes are
denied unless the device has `tx_enabled = true` in config. That flag is only set after
the protocol for that exact device/firmware has been verified and the owner approved it.

This is defence in depth: the CAN interfaces are also configured listen-only at the
kernel level (systemd-networkd `ListenOnly=yes`), so a bug here still cannot put frames
on a heater bus.
"""

from __future__ import annotations

import logging

log = logging.getLogger(__name__)


class TxBlocked(PermissionError):
    pass


class TxGate:
    def __init__(self, enabled: dict[str, bool] | None = None) -> None:
        self._enabled = dict(enabled or {})
        self.blocked_count: dict[str, int] = {}

    def is_enabled(self, device: str) -> bool:
        return self._enabled.get(device, False) is True

    def check(self, device: str, what: str, quiet: bool = False) -> None:
        """quiet: routine protocol housekeeping (e.g. answering a peer's address-claim poll) is logged at DEBUG
        when allowed; a BLOCKED transmit is always a warning."""
        if self.is_enabled(device):
            (log.debug if quiet else log.info)("TX allowed  %s: %s", device, what)
            return
        self.blocked_count[device] = self.blocked_count.get(device, 0) + 1
        log.warning("TX BLOCKED %s: %s (tx_enabled is false)", device, what)
        raise TxBlocked(f"transmit to {device!r} is disabled")
