"""Central state store: a flat dict of dotted keys -> JSON-able values, with pub/sub.

Adapters write with `set()`. The web layer subscribes and receives coalesced changes.
"""

from __future__ import annotations

import asyncio
import time
from typing import Any


class State:
    def __init__(self) -> None:
        self._values: dict[str, Any] = {}
        self._stamps: dict[str, float] = {}
        self._subs: set[asyncio.Queue[dict[str, Any]]] = set()

    def set(self, key: str, value: Any) -> None:
        if self._values.get(key, _MISSING) == value:
            self._stamps[key] = time.time()
            return
        self._values[key] = value
        self._stamps[key] = time.time()
        for q in self._subs:
            try:
                q.put_nowait({key: value})
            except asyncio.QueueFull:
                pass  # slow consumer; it will resync from snapshot()

    def update(self, values: dict[str, Any]) -> None:
        for k, v in values.items():
            self.set(k, v)

    def get(self, key: str, default: Any = None) -> Any:
        return self._values.get(key, default)

    def age(self, key: str) -> float | None:
        ts = self._stamps.get(key)
        return None if ts is None else time.time() - ts

    def snapshot(self) -> dict[str, Any]:
        return dict(self._values)

    def subscribe(self, maxsize: int = 4096) -> asyncio.Queue[dict[str, Any]]:
        q: asyncio.Queue[dict[str, Any]] = asyncio.Queue(maxsize=maxsize)
        self._subs.add(q)
        return q

    def unsubscribe(self, q: asyncio.Queue[dict[str, Any]]) -> None:
        self._subs.discard(q)


_MISSING = object()
