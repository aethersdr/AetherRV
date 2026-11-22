"""N2K switching: wire format against real captures, the gated controller, and keypad/input logic."""
import asyncio
import time

import pytest

from aetherrv.adapters.n2k import N2kAdapter
from aetherrv.state import State
from aetherrv.switching import (N2kNode, SwitchController, can_id, dim_command, fast_packet_frames,
                                switch_command)
from aetherrv.txgate import TxGate

H = bytes.fromhex


# ── wire format, byte for byte against N2KAnalyzer → CLMD16 captures (fw 1.0.3.66, 2026-11-21) ──
def test_switch_on_matches_capture():
    # 01ED0028#600A010DF201F802 / 01ED0028#6101200201FFFFFF : bank 32, channel 1 ON, seq 3
    frames = fast_packet_frames(switch_command(32, 1, True), seq=3)
    assert frames == [H("600A010DF201F802"), H("6101200201FFFFFF")]


def test_switch_off_and_other_channels_match_capture():
    assert fast_packet_frames(switch_command(32, 1, False), 1)[1] == H("2101200200FFFFFF")
    assert switch_command(32, 2, True).hex() == "010df201f80201200301"      # field 3 = channel 2
    assert switch_command(32, 8, True).hex() == "010df201f80201200901"      # field 9 = channel 8


def test_dim_matches_capture():
    # "f8 02 02 0b 06 64": connection 11 (= channel 12), duty 100 %
    assert dim_command(12, 100).hex() == "010cf201f802020b0664"
    assert dim_command(12, 33)[-1] == 33
    with pytest.raises(ValueError):
        dim_command(12, 101)


def test_identifiers():
    assert can_id(3, 126208, 150, dest=0) == 0x0DED0096          # PDU1 + data page 1; = CLMD16's own ACK 0x0DED2800
    assert can_id(6, 60928, 150, dest=255) == 0x18EEFF96
    assert can_id(0, 126208, 0x28, dest=0) == 0x01ED0028         # the IPG100's own frame


# ── fakes ──
class FakeBus:
    def __init__(self):
        self.sent = []

    def send(self, device, msg, gate, quiet=False):
        gate.check(device, "test", quiet=quiet)
        self.sent.append(msg)


def setup(tx=True):
    s = State()
    n2k = N2kAdapter(s)
    gate = TxGate({"n2k": tx})
    bus = FakeBus()
    node = N2kNode(bus, gate, preferred=150)
    cfg = {"switching": {"controller": 0, "bank": 32, "vsw_bank": 33, "press_s": 0.05},
           "switch": [{"bank": 32, "channel": 1, "name": "Spot lights", "press": 1},
                      {"bank": 32, "channel": 4, "name": "Reverse lights", "press": 4},
                      {"bank": 32, "channel": 7, "name": "Dump valve", "press": 9},
                      {"bank": 32, "channel": 9, "name": "Drawer locks", "press": 3, "blocked": True,
                       "blocked_reason": "drawer locks are blocked until the CLMD16 cut-off timer is back"},
                      {"bank": 32, "channel": 12, "name": "Cabin lights", "kind": "dimmer", "press": 12},
                      {"bank": 32, "channel": 16, "name": "Spare"}],
           "input": [{"input": 4, "name": "Reverse"}, {"input": 6, "name": "Dome", "debounce_s": 1.5}]}
    ctl = SwitchController(s, n2k, node, gate, cfg)
    ctl.VERIFY = 0.4
    return s, n2k, node, bus, ctl


def status(n2k, src, inst, on=(), now=None):
    st = ["off"] * 28
    for c in on:
        st[c - 1] = "on"
    n2k.banks[(src, inst)] = {"source": src, "instance": inst, "states": st, "seen": now or time.time()}
    for cb in n2k.bank_listeners:
        cb(src, inst, st, now or time.time())


def claimed(node):
    node.claim()
    node.claimed_at -= 1


def sent_payloads(bus):
    return [bytes(bus.sent[i].data)[2:] + bytes(bus.sent[i + 1].data)[1:] for i in range(0, len(bus.sent), 2)]


def ready(tx=True, on=(), vsw_on=()):
    s, n2k, node, bus, ctl = setup(tx)
    claimed(node); bus.sent.clear()
    status(n2k, 0, 32, on=on); status(n2k, 0, 33, on=vsw_on)
    return s, n2k, node, bus, ctl


def press(switch, on):
    return switch_command(33, switch, on)


# ── controller: a set is a virtual-switch press, never a direct breaker command (owner 2026-11-22) ──
async def test_set_presses_the_virtual_switch_once_and_verifies():
    s, n2k, node, bus, ctl = ready()

    async def clmd16():                       # its own logic switches ch1 on after the press
        await asyncio.sleep(0.1); status(n2k, 0, 32, on=[1])

    asyncio.get_running_loop().create_task(clmd16())
    r = await ctl.command("set", {"channel": 1, "on": True}, client="test")
    assert r["ok"], r
    assert [p[:10] for p in sent_payloads(bus)] == [press(1, True), press(1, False)]     # ON, then OFF; nothing else
    assert bus.sent[0].arbitration_id == 0x0DED0096


async def test_never_sends_to_the_breaker_bank():
    s, n2k, node, bus, ctl = ready(on=[12])
    for cmd, val in (("set", {"channel": 1, "on": True}), ("set", {"channel": 12, "on": False}),
                     ("set", {"channel": 7, "on": True}), ("dim", {"channel": 12, "level": 50})):
        await ctl.command(cmd, val)
    assert sent_payloads(bus) and all(p[:10] != switch_command(32, ch, on)
                                      for p in sent_payloads(bus) for ch in range(1, 17) for on in (True, False))


async def test_dim_is_refused_for_now():
    s, n2k, node, bus, ctl = ready(on=[12])
    r = await ctl.command("dim", {"channel": 12, "level": 50})
    assert not r["ok"] and "Nothing was sent" in r["detail"] and bus.sent == []


async def test_channel_without_a_virtual_switch_is_refused():
    s, n2k, node, bus, ctl = ready()
    r = await ctl.command("set", {"channel": 16, "on": True})
    assert not r["ok"] and "no virtual switch" in r["detail"] and bus.sent == []


async def test_blocked_when_tx_disabled_and_nothing_sent():
    s, n2k, node, bus, ctl = setup(tx=False)
    status(n2k, 0, 32); status(n2k, 0, 33)
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert not r["ok"] and bus.sent == [] and s.get("n2k.controls_ok") is False


async def test_needs_address_claim_and_fresh_status():
    s, n2k, node, bus, ctl = setup()
    status(n2k, 0, 32); status(n2k, 0, 33)
    assert not (await ctl.command("set", {"channel": 1, "on": True}))["ok"]          # no address yet
    claimed(node)
    bus.sent.clear()
    status(n2k, 0, 32, now=time.time() - 10)                                          # stale breakers
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert not r["ok"] and "fresh" in r["detail"] and bus.sent == []
    status(n2k, 0, 32); status(n2k, 0, 33, now=time.time() - 10)                      # stale virtual switches
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert not r["ok"] and "virtual switches" in r["detail"] and bus.sent == []


async def test_blocked_channel_refuses_on():
    s, n2k, node, bus, ctl = ready()
    r = await ctl.command("set", {"channel": 9, "on": True})
    assert not r["ok"] and "cut-off" in r["detail"] and bus.sent == []


async def test_not_switched_by_the_controller_is_reported_and_not_retried():
    s, n2k, node, bus, ctl = ready()
    r = await ctl.command("set", {"channel": 4, "on": True})     # the CLMD16's logic doesn't act
    assert not r["ok"] and "didn't switch" in r["detail"]
    assert [p[:10] for p in sent_payloads(bus)] == [press(4, True), press(4, False)]


async def test_noop_when_already_in_state():
    s, n2k, node, bus, ctl = ready(on=[1])
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert r["ok"] and bus.sent == []


async def test_refuses_while_its_virtual_switch_is_already_on():
    s, n2k, node, bus, ctl = ready(vsw_on=[1])
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert not r["ok"] and "already pressed" in r["detail"] and bus.sent == []


async def test_release_stuck_releases_only_our_switches_that_are_on():
    s, n2k, node, bus, ctl = ready(vsw_on=[4, 5, 12])          # 5 isn't ours
    assert ctl.owned_switches() == [1, 3, 4, 9, 12]
    stuck = await ctl.release_stuck(wait=0.2)
    assert stuck == [4, 12]
    assert [p[:10] for p in sent_payloads(bus)] == [press(4, False), press(12, False)]


async def test_release_stuck_skips_without_status():
    s, n2k, node, bus, ctl = setup()
    claimed(node); bus.sent.clear()
    assert await ctl.release_stuck(wait=0.2) == [] and bus.sent == []


async def test_keypad_presses_never_send():
    """CKM12 presses are the CLMD16's job (dual control, owner 2026-11-22): AetherRV must not relay them,
    even with automation on and an old [[binding]] table still in the config."""
    s, n2k, node, bus, ctl = setup()
    ctl2 = SwitchController(s, n2k, node, ctl.gate, {"switching": {"controller": 0, "bank": 32, "automation": True},
                                                     "binding": [{"keypad": 101, "button": 9, "channels": [7, 16]}]})
    claimed(node); bus.sent.clear()
    status(n2k, 0, 32)
    for on in ([], [9], [], [9], []):
        status(n2k, 101, 50, on=on)
    await asyncio.sleep(0.05)
    assert bus.sent == [] and not hasattr(ctl2, "bindings")


async def test_inputs_are_read_but_never_switch_anything():
    """Reverse follow and ignition auto-off moved to the CLMD16 (owner 2026-11-22): input changes are
    debounced and shown, but AetherRV sends nothing, even with an old `follow` / `automation` config."""
    s, n2k, node, bus, ctl = setup()
    ctl2 = SwitchController(s, n2k, node, ctl.gate, {
        "switching": {"controller": 0, "bank": 32, "automation": True},
        "switch": [{"bank": 32, "channel": 1, "name": "Spot", "requires_input": 2}],
        "input": [{"input": 2, "name": "Ignition"}, {"input": 4, "name": "Reverse", "follow": [4]}]})
    claimed(node); bus.sent.clear()
    t0 = time.time()
    status(n2k, 0, 32, on=[18, 20, 1], now=t0)        # ignition + reverse on, spot on
    ctl2.tick(t0 + 1)
    status(n2k, 0, 32, on=[1], now=t0 + 2)            # ignition and reverse go off
    ctl2.tick(t0 + 2.5)
    ctl2.tick(t0 + 3)
    await asyncio.sleep(0.05)
    assert bus.sent == []
    assert [(i["name"], i["on"]) for i in s.get("switching.inputs")] == [("Ignition", False), ("Reverse", False)]
    assert s.get("switching.automation") is False


def test_address_claim_moves_off_a_contested_address():
    s, n2k, node, bus, ctl = setup()
    node.claim()
    assert node.address == 150 and bus.sent[-1].arbitration_id == 0x18EEFF96
    import can
    lower = (node.name - 1).to_bytes(8, "little")      # a device with a lower (stronger) NAME claims 150
    node.on_frame(can.Message(arbitration_id=0x18EEFF96, data=lower, is_extended_id=True))
    assert node.address != 150 and bus.sent[-1].arbitration_id == (0x18EEFF00 | node.address)


async def test_tripped_channel_is_refused():
    s, n2k, node, bus, ctl = ready()
    n2k.banks[(0, 32)]["states"][3] = "error"               # ch4 tripped
    for on in (True, False):
        r = await ctl.command("set", {"channel": 4, "on": on})
        assert not r["ok"] and "tripped" in r["detail"]
    assert bus.sent == []


async def test_ignition_interlock_refuses_on_until_ignition_and_explains_a_refusal():
    s, n2k, node, bus, ctl = ready()
    ctl.channels[1]["requires_input"] = 2
    ctl.inputs[2] = {"input": 2, "name": "Ignition"}
    from aetherrv.switching import Debounced
    ctl.deb[2] = Debounced()
    r = await ctl.command("set", {"channel": 1, "on": True})
    assert not r["ok"] and "Ignition" in r["detail"] and bus.sent == []
    ctl.deb[2].value = True
    r = await ctl.command("set", {"channel": 1, "on": True})       # pressed; the fake CLMD16 doesn't act
    assert not r["ok"] and len(sent_payloads(bus)) == 2


def setup_trigger():
    s, n2k, node, bus, ctl = setup()
    from aetherrv.switching import Trigger
    ctl.triggers["drawer_locks"] = Trigger("drawer_locks", "Drawer locks", bank=33, switch=1, channel=11,
                                           pulse_s=0.2, expect_off_s=0.8)
    claimed(node); bus.sent.clear()
    return s, n2k, node, bus, ctl


async def test_trigger_pulses_virtual_switch_and_controller_timer_cuts_off():
    s, n2k, node, bus, ctl = setup_trigger()
    status(n2k, 0, 32); status(n2k, 0, 33)

    async def clmd16():                       # the controller: vsw ON → ch11 on; then its timer turns ch11 off
        await asyncio.sleep(0.05); status(n2k, 0, 33, on=[1]); status(n2k, 0, 32, on=[11])
        await asyncio.sleep(0.3); status(n2k, 0, 33)
        await asyncio.sleep(0.2); status(n2k, 0, 32)

    asyncio.get_running_loop().create_task(clmd16())
    r = await ctl.command("trigger", {"trigger": "drawer_locks"})
    assert r["ok"]
    p = sent_payloads(bus)
    assert p[0][:10] == switch_command(33, 1, True) and p[1][:10] == switch_command(33, 1, False)
    await asyncio.sleep(1.0)
    assert len(sent_payloads(bus)) == 2                          # no safety-off needed
    assert s.get("switching.trigger.drawer_locks")["ok"] is True


async def test_trigger_safety_net_switches_output_off_if_controller_does_not():
    s, n2k, node, bus, ctl = setup_trigger()
    status(n2k, 0, 32); status(n2k, 0, 33)

    async def broken_timer():                 # vsw works, ch11 comes on, but never goes off
        await asyncio.sleep(0.05); status(n2k, 0, 33, on=[1]); status(n2k, 0, 32, on=[11])
        for _ in range(12):
            await asyncio.sleep(0.1); status(n2k, 0, 32, on=[11]); status(n2k, 0, 33)

    asyncio.get_running_loop().create_task(broken_timer())
    await ctl.command("trigger", {"trigger": "drawer_locks"})
    await asyncio.sleep(1.0)
    p = sent_payloads(bus)
    assert p[-1][:10] == switch_command(32, 11, False)           # safety net: direct OFF to the output
    assert s.get("switching.trigger.drawer_locks")["ok"] is False


async def test_plain_on_to_trigger_channel_stays_blocked():
    s, n2k, node, bus, ctl = setup_trigger()
    ctl.channels[11] = {"channel": 11, "name": "Drawer locks", "blocked": True, "blocked_reason": "use the trigger"}
    status(n2k, 0, 32); status(n2k, 0, 33)
    r = await ctl.command("set", {"channel": 11, "on": True})
    assert not r["ok"] and bus.sent == []


def test_claim_poll_reply_is_sent_but_logged_quietly(caplog):
    import logging
    import can
    from aetherrv.switching import N2kNode, PGN_ADDRESS_CLAIM, can_id
    from aetherrv.txgate import TxGate
    bus = FakeBus()
    node = N2kNode(bus, TxGate({"n2k": True}))
    with caplog.at_level(logging.INFO):
        node.claim()                                            # first claim: visible
        assert any("claiming address 150 (claim)" in r.message for r in caplog.records)
        caplog.clear()
        req = can.Message(arbitration_id=can_id(6, 59904, 228, 150), data=PGN_ADDRESS_CLAIM.to_bytes(3, "little"),
                          is_extended_id=True)
        node.on_frame(req)                                      # the Cerbo's 10-s poll
        assert len(bus.sent) == 2                               # still answered
        assert not [r for r in caplog.records if r.levelno >= logging.INFO]   # but not at INFO
