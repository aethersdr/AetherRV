#!/usr/bin/env python3
"""Pair AetherRV with a Victron GX (Venus OS >= 3.80 "MQTT pairing") and store the MQTT credentials.

Run ON THE PI as root (it writes /etc/aetherrv/secrets.toml):
    sudo /opt/aetherrv/.venv/bin/python /opt/aetherrv/tools/victron_pair.py <cerbo-ip>

Then, on the Cerbo: Settings → Integrations → MQTT → enable pairing mode (open for 120 s).
This tool retries POST https://<gx>/auth/generate-token/ (form: role, device_id) every 5 s for up to
4 minutes, the same request the victron-mqtt library (Apache-2.0, used by Home Assistant) makes.
On success it writes victron_mqtt_user / victron_mqtt_password (0640 root:aetherrv). The password
is never printed.
"""

import asyncio
import json
import os
import socket
import sys
import time

import aiohttp

SECRETS = "/etc/aetherrv/secrets.toml"


async def try_pair(host: str, device_id: str, role: str) -> dict | None:
    async with aiohttp.ClientSession(connector=aiohttp.TCPConnector(ssl=False),
                                     timeout=aiohttp.ClientTimeout(total=8)) as s:
        async with s.post(f"https://{host}/auth/generate-token/", data={"role": role, "device_id": device_id}) as r:
            body = await r.text()
            if r.status == 200:
                return json.loads(body)
            print(f"  not yet: HTTP {r.status} {body[:120].strip()}")
            return None


def store(user: str, password: str) -> None:
    try:
        lines = [l for l in open(SECRETS).read().splitlines() if not l.startswith(("victron_mqtt_user", "victron_mqtt_password"))]
    except FileNotFoundError:
        lines = []
    lines += [f"victron_mqtt_user = {json.dumps(user)}", f"victron_mqtt_password = {json.dumps(password)}"]
    with open(SECRETS, "w") as f:
        f.write("\n".join(lines) + "\n")
    import grp
    os.chown(SECRETS, 0, grp.getgrnam("aetherrv").gr_gid)
    os.chmod(SECRETS, 0o640)


def port_open(host: str, port: int) -> bool:
    try:
        socket.create_connection((host, port), timeout=3).close()
        return True
    except OSError:
        return False


async def main() -> None:
    if len(sys.argv) < 2:
        sys.exit("usage: victron_pair.py <cerbo-ip-or-host> [role]")
    host = sys.argv[1]
    device_id = "aetherrv" + socket.gethostname().lower().replace("-", "")
    role = sys.argv[2] if len(sys.argv) > 2 else "aetherrv"
    print(f"Waiting for pairing mode on {host} (device_id={device_id}, role={role}). Enable it on the Cerbo now…")
    deadline = time.time() + 240
    while time.time() < deadline:
        try:
            res = await try_pair(host, device_id, role)
        except (aiohttp.ClientError, asyncio.TimeoutError) as e:
            print(f"  not yet: {e}")
            res = None
        if res:
            store(res["token_name"], res["password"])
            print(f"Paired. MQTT username = {res['token_name']} (password stored in {SECRETS}, 0640)")
            for p in (1883, 8883):
                print(f"  MQTT port {p}: {'open' if port_open(host, p) else 'closed'}")
            return
        await asyncio.sleep(5)
    sys.exit("Timed out: pairing mode was not detected within 4 minutes.")


if __name__ == "__main__":
    asyncio.run(main())
