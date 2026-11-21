from aetherrv.adapters.nmea_gps import NmeaGps, checksum_ok, parse
from aetherrv.state import State


def nmea(body: str) -> str:
    cs = 0
    for ch in body:
        cs ^= ord(ch)
    return f"${body}*{cs:02X}"


# Classic reference sentences (widely published examples)
GGA = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47"
RMC = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A"


def test_checksums():
    assert checksum_ok(GGA) and checksum_ok(RMC)
    assert not checksum_ok(GGA.replace("*47", "*48"))


def test_parse_reference_sentences():
    k, g = parse(GGA)
    assert k == "GGA" and g["sats"] == 8 and g["hdop"] == 0.9 and g["alt_m"] == 545.4
    assert round(g["lat"], 4) == 48.1173 and round(g["lon"], 4) == 11.5167
    k, r = parse(RMC)
    assert k == "RMC" and r["valid"] and r["speed_kn"] == 22.4 and r["course"] == 84.4


def test_southern_western_and_gn_talker():
    s = nmea("GNRMC,010203.00,A,4522.4100,N,12141.7540,W,0.0,0.0,201126,,,A")
    k, r = parse(s)
    assert round(r["lat"], 5) == 45.37350 and round(r["lon"], 5) == -121.69590
    k, r = parse(nmea("GPRMC,010203,A,3351.000,S,15112.000,E,0,0,201126,,"))
    assert r["lat"] < 0 and r["lon"] > 0


def test_listener_updates_state_and_no_fix():
    st = State()
    g = NmeaGps(st)
    g.datagram_received((GGA + "\r\n" + RMC + "\r\n").encode(), ("192.0.2.1", 5000))
    assert st.get("location.fix") is True and st.get("location.sats") == 8
    assert st.get("location.speed") == round(22.4 * 1.852, 1) and st.get("location.source") == "peplink NMEA"
    g.feed(nmea("GPRMC,123520,V,,,,,,,230394,,"))
    assert st.get("location.fix") is False
    g.feed("$GPGGA,garbage*00")
    assert g.bad == 1
