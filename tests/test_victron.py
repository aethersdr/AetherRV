from aetherrv.adapters.victron import VictronAdapter
from aetherrv.state import State


def feed(a, inst, **fields):
    for k, v in fields.items():
        a._on_temp(f"temperature/{inst}/{k}", v)


def test_temps_auto_pick_by_type():
    s = State()
    a = VictronAdapter(s, "x")
    feed(a, "20", TemperatureType=0, Temperature=12.0)          # battery
    feed(a, "21", TemperatureType=3, Temperature=19.5, Humidity=41.0, CustomName="Cabin")
    feed(a, "22", TemperatureType=4, Temperature=-3.0)
    assert s.get("climate.cabin.temp") == 19.5 and s.get("climate.cabin.humidity") == 41.0
    assert s.get("climate.outside.temp") == -3.0
    assert len(s.get("victron.temps")) == 3


def test_temps_explicit_by_name_or_instance():
    s = State()
    a = VictronAdapter(s, "x", cabin_temp="Galley", outside_temp=30)
    feed(a, "29", TemperatureType=3, Temperature=25.0)          # a "room" sensor that is NOT the chosen one
    feed(a, "31", CustomName="Galley", Temperature=21.0)
    feed(a, "30", Temperature=5.0)
    assert s.get("climate.cabin.temp") == 21.0
    assert s.get("climate.outside.temp") == 5.0


def test_four_ruuvi_tags_by_name():
    s = State()
    a = VictronAdapter(s, "x", cabin_temp="Inside Temp", outside_temp="Outside Temp",
                       fridge_temp="Fridge Temp", freezer_temp="freezer temp")
    for inst, name, temp in [("20", "Inside Temp", 20.5), ("21", "Outside Temp", -1.0),
                             ("22", "Fridge Temp", 3.5), ("23", "Freezer Temp", -18.0)]:
        feed(a, inst, TemperatureType=2, CustomName=name, Temperature=temp)  # Ruuvi as "generic"
    feed(a, "20", Humidity=44.0)
    assert [s.get(f"climate.{r}.temp") for r in ("cabin", "outside", "fridge", "freezer")] == [20.5, -1.0, 3.5, -18.0]
    assert s.get("climate.cabin.humidity") == 44.0



def test_battery_collection_and_kind():
    s = State()
    a = VictronAdapter(s, "x")
    for path, v in [("battery/512/System/NrOfModulesOnline", 1), ("battery/512/Info/MaxChargeCurrent", 230.0),
                    ("battery/512/Alarms/CellImbalance", 0), ("battery/512/Alarms/LowVoltage", 1),
                    ("battery/0/ProductName", "Lynx Shunt 1000A VE.Can"), ("battery/0/Soc", 85.8),
                    ("battery/0/Some/Other/Path", 5)]:
        a._on_battery(path, v)
    bl = {b["instance"]: b for b in a.batteries()}
    assert bl["512"]["kind"] == "bms" and bl["512"]["active_alarms"] == {"LowVoltage": 1}
    assert bl["0"]["kind"] == "monitor" and bl["0"]["Soc"] == 85.8 and "Some/Other/Path" not in bl["0"]


async def test_resubscribes_after_reconnect():
    """Clean-session MQTT: after a reconnect the data topics must be subscribed again (they were not → no data)."""
    class C:
        def __init__(self): self.subs = []
        async def subscribe(self, t): self.subs.append(t)
        async def publish(self, t, p=b""): pass
    s = State(); a = VictronAdapter(s, "x")
    for _ in range(2):                                  # first connection, then a reconnect
        c = C(); a._subscribed = False                  # what run() does on each connect
        await a._on_message(c, "N/abc123/system/0/Serial", b'{"value": "abc123"}')
        assert "N/abc123/battery/#" in c.subs
    c = C(); a._subscribed = False                      # a different Cerbo on the same address is ignored
    await a._on_message(c, "N/zzz999/system/0/Serial", b'{"value": "zzz999"}')
    assert c.subs == [] and s.get("victron.portal") == "abc123"
