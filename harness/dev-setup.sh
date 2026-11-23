#!/bin/bash
# Workstation / bench test setup (needs root): virtual CAN interfaces vcan0-vcan15 for the tests and simulators.
# Mosquitto must be installed (tests start private instances; the system unit stays disabled).
set -euo pipefail
modprobe vcan
for i in $(seq 0 15); do
  ip link show "vcan$i" >/dev/null 2>&1 || ip link add "vcan$i" type vcan
  ip link set "vcan$i" up
done
command -v mosquitto >/dev/null || echo "warning: mosquitto is not installed" >&2
echo "vcan0-vcan15 up"
