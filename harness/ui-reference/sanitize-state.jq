# Sanitize an /api/state snapshot for PUBLIC screenshots (README): run on the workstation, review, then render with
# harness/ui-reference/screenshots.sh. Rules, not values: nothing private is written here.
#   jq -f harness/ui-reference/sanitize-state.jq state-raw.json > state.json
# - the position is Denver (the README's stand-in place), the time zone America/Denver
# - Wi-Fi network names become "Campground Wi-Fi" wherever they appear (the WAN's ssid and its message)
# - the Bluetooth batteries are "Batt 1"…"Batt N" (their names are serial tails; alerts use them too)
# - serial numbers are zeroed; MAC addresses and private IPv4 addresses are replaced with documentation values

def ssids: [.["peplink.wans"][]? | .wifi.ssid? // empty | select(. != "")] | unique;
def batteries: [.["batteries.ble"][]? | .name | tostring] | to_entries | map({key: .value, value: (.key + 1 | tostring)}) | from_entries;

. as $in
| ($in | ssids) as $ssids
| ($in | batteries) as $batt
| .["location.lat"] = 39.7392 | .["location.lon"] = -104.9903 | .["location.alt_m"] = 1609.0
| if has("settings.timezone") then .["settings.timezone"] = "America/Denver" else . end
| .["batteries.ble"] |= (if type == "array" then [to_entries[] | .value + {name: "Batt \(.key + 1)"}] else . end)
| walk(
    if type == "object" then with_entries(if (.key | test("^serial$"; "i")) and (.value | type) == "string" and .value != ""
                                          then .value = "0000000" else . end)
    elif type == "string" then
      reduce $ssids[] as $s (.; split($s) | join("Campground Wi-Fi"))
      | reduce ($batt | to_entries[]) as $b (.; gsub("\\bBattery \($b.key)\\b"; "Batt \($b.value)")
                                               | gsub("\\bepoch\\.\($b.key)\\."; "epoch.batt\($b.value)."))
      | gsub("\\b([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\\b"; "AA:00:00:00:00:00")
      # private and Tailscale IPv4 (10/8, 172.16/12, 192.168/16, 100.64/10): keep the last octet
      | gsub("\\b(10\\.\\d+|172\\.(1[6-9]|2\\d|3[01])|192\\.168|100\\.(6[4-9]|[7-9]\\d|1[01]\\d|12[0-7]))\\.\\d+\\.(?<d>\\d+)\\b"; "192.0.2.\(.d)")
    else . end)
