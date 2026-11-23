#!/usr/bin/env python3
"""Create the Peplink read-write API client AetherRV uses ONLY for WAN priority changes.

Run on the Pi, as root, in an interactive terminal:
    sudo python3 -I /opt/aetherrv/deploy/peplink-rw-client.py [router-ip]

You type the router's ADMIN username and password; they are used for this one session and never stored
or printed. The script:
  1. logs in to the router (POST /api/login),
  2. creates an API client with scope "api" (read-write) named "AetherRV WAN priority",
  3. checks the new client can get a token,
  4. writes peplink_rw_client_id / peplink_rw_client_secret into /etc/aetherrv/secrets.toml (backup first),
  5. prints the shape of config.wan.connection.priority (no secrets), then logs out.
"""
import getpass
import json
import os
import shutil
import ssl
import sys
import time
import urllib.request

SECRETS = "/etc/aetherrv/secrets.toml"
NAME = "AetherRV WAN priority"


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: sudo peplink-rw-client.py <router-lan-ip>")
        return 2
    host = sys.argv[1]
    if os.geteuid() != 0:
        print("Run with sudo (it writes /etc/aetherrv/secrets.toml).")
        return 1
    base = f"https://{host}/api"
    ctx = ssl.create_default_context()
    ctx.check_hostname, ctx.verify_mode = False, ssl.CERT_NONE        # the router's self-signed certificate
    op = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))
    cookies: dict[str, str] = {}       # kept by hand: Python's cookie jar drops the router's bare-IP session cookie

    def call(func, body=None, **q):
        url = f"{base}/{func}" + ("?" + "&".join(f"{k}={v}" for k, v in q.items()) if q else "")
        data = None if body is None else json.dumps(body).encode()
        hdr = {"Content-Type": "application/json"}
        if cookies:
            hdr["Cookie"] = "; ".join(f"{k}={v}" for k, v in cookies.items())
        with op.open(urllib.request.Request(url, data=data, headers=hdr), timeout=15) as r:
            for sc in r.headers.get_all("Set-Cookie") or []:
                k, _, v = sc.split(";", 1)[0].partition("=")
                if k.strip():
                    cookies[k.strip()] = v.strip()
            return json.loads(r.read() or b"{}")

    print(f"Peplink router {host}: create the read-write API client for AetherRV's WAN priority control.")
    user = input("Router admin username: ").strip()
    pw = getpass.getpass("Router admin password (not shown): ")
    r = call("login", {"username": user, "password": pw})
    pw = None
    if r.get("stat") != "ok":
        print(f"Login failed: {r.get('code')} {r.get('message')}")
        return 1
    if not ((r.get("response") or {}).get("permission") or {}).get("POST"):
        print("That login has no write permission; use the router's admin account.")
        call("logout", {})
        return 1
    try:
        chk = call("status.wan.connection")              # harmless read: proves the session is carried
        if chk.get("stat") != "ok":
            print(f"Logged in, but the session isn't accepted ({chk.get('code')} {chk.get('message')}; "
                  f"cookies seen: {sorted(cookies)}). Nothing created.")
            return 1
        print("Session check: OK.")
        r = call("auth.client", {"action": "add", "name": NAME, "scope": "api"})
        if r.get("stat") != "ok":
            print(f"Creating the API client failed: {r.get('code')} {r.get('message')}")
            return 1
        resp = r.get("response") or {}
        cid, csec = resp.get("clientId"), resp.get("clientSecret")
        if not (cid and csec):
            print(f"Unexpected reply (fields: {sorted(resp)}); nothing written.")
            return 1
        print(f"Created API client \"{NAME}\" (read-write).")

        g = call("auth.token.grant", {"clientId": cid, "clientSecret": csec, "scope": "api"})
        if g.get("stat") != "ok":
            print(f"The new client can't get a token: {g.get('code')} {g.get('message')}; nothing written.")
            return 1
        tok = g["response"]["accessToken"]
        print("Token check: OK.")

        shutil.copy2(SECRETS, f"{SECRETS}.pre-peplink-rw-{time.strftime('%Y%m%d-%H%M%S')}")
        st = os.stat(SECRETS)
        lines = [l for l in open(SECRETS).read().splitlines()
                 if not l.strip().startswith(("peplink_rw_client_id", "peplink_rw_client_secret"))]
        lines += [f'peplink_rw_client_id = "{cid}"', f'peplink_rw_client_secret = "{csec}"']
        tmp = SECRETS + ".tmp"
        with open(tmp, "w") as f:
            f.write("\n".join(lines) + "\n")
        os.chown(tmp, st.st_uid, st.st_gid)
        os.chmod(tmp, st.st_mode & 0o777)
        os.replace(tmp, SECRETS)
        print(f"Saved to {SECRETS} (backup kept next to it).")

        p = call("config.wan.connection.priority", accessToken=tok)
        print("\nRouter's WAN priority list (for the format check):")
        print(json.dumps(p.get("response", p), indent=2)[:2000])
        try:
            call("auth.token.revoke", {"clientId": cid, "accessToken": tok})
        except Exception:
            pass                                  # the token expires on its own
        print("\nDone. Tell Claude it's set up.")
        return 0
    finally:
        try:
            call("logout", {})
        except Exception:
            pass


if __name__ == "__main__":
    sys.exit(main())
