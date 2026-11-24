# AetherRV build guide

> [!CAUTION]
> **AetherRV can switch real loads and run a diesel heater.** A wiring mistake on a CAN bus can stop your
> heater, your switching or your battery communications, and a bad command can turn on something you didn't
> mean to. AetherRV is built to fail safe (every device starts listen-only, every write is refused until you
> enable it, and the devices keep their own safety logic), but you are responsible for your van. Do electrical
> work with the power off and fused. AetherRV comes with no warranty of any kind (see `LICENSE`), and you
> follow this guide entirely at your own risk.

## Before you start

**What you're building.** A 10.1" touchscreen console on a Raspberry Pi 5 that shows and controls the van's
power, heat, network and switching on one screen. It talks to the van's equipment over the network (Victron
Cerbo GX, Rixens MCS7, Starlink, Peplink), over CAN (NMEA 2000 switching, an Espar heater) and over Bluetooth
(lithium batteries). See the [README](../README.md) for screenshots and the full feature list.

**Your van will be different.** The reference build is one Sprinter with one particular set of equipment. Every
integration is optional, and the [last section](#make-it-yours-prompts-for-your-ai) has prompts for adapting
AetherRV to the equipment you have.

**How to use this guide.** Do the steps in order. Each step says what's happening, gives the commands, says
what success looks like, and ends with a **prompt** you can paste into an AI assistant that can run commands on
your computer (Claude Code, for example). You do the physical work and you type every password yourself; the
assistant can do the rest while you check the results.

**Time:** an afternoon for the Pi and the screen, then an evening per device you integrate.

## What you need

### The console

| Item | Notes |
|---|---|
| **Raspberry Pi 5**, 8 GB or 16 GB | The reference build uses 16 GB. 4 GB works but leaves little room for Chromium. |
| **Active cooler** for the Pi 5 | The official one, or any Pi 5 cooler that leaves the 40-pin header free. |
| **Waveshare 10.1" HDMI LCD (H)** | 1920×1200 IPS, HDMI video + USB touch. Other HDMI touchscreens work; see the prompts at the end for a different size. |
| Micro-HDMI to HDMI cable, and a USB-A cable for touch | Short ones; the screen and Pi usually sit together. |
| **Waveshare 2-CH CAN HAT+** | Two isolated CAN 2.0 ports (MCP2515). One for NMEA 2000, one for the Espar heater. Needed only for CAN devices. |
| **12/24 V → 5 V 5 A USB-C step-down converter** | The reference build uses [this waterproof 5 A buck module](https://www.amazon.com/dp/B0CRVVWL4Y). The Pi 5 wants 5 V at 5 A; feed the converter from a fused 12 V circuit. |
| **NVMe SSD + adapter** (optional, recommended) | The reference build uses the Waveshare NVMe + PoE HAT (the PoE part is unused) with a small NVMe SSD. The history database and CAN captures write constantly, which wears out SD cards; an NVMe drive doesn't mind. A good SD card works if you skip it. |
| A computer running Linux, and a USB NVMe enclosure (or SD card reader) | To install the system before the drive goes into the Pi. |

### Optional extras

| Item | Notes |
|---|---|
| **Waveshare 2-CH CAN FD HAT** | Two more CAN ports, stacked on the CAN HAT+. Only if you have more than two CAN buses. |
| An NMEA 2000 drop cable | Cut it to wire the N2K backbone to the CAN HAT (step 8). |
| A Tailscale account (free) | To reach the console from your phone or from home. |
| A TomTom developer key (free tier) | Traffic on the weather map. |

### Equipment AetherRV can talk to

None of these is required; each one you have lights up part of the screen.

| Equipment | How | What you get |
|---|---|---|
| Victron Cerbo GX (or any Venus OS GX) | MQTT over the LAN | Battery, solar, shore, alternator, inverter, loads, tank and Ruuvi temperature sensors; MultiPlus mode, shore limit, MPPT on/off |
| Lithium batteries with a RoyPow-protocol BMS (e.g. EPOCH) | Bluetooth LE | Per-battery state of charge, cells, temperatures, faults |
| Rixens MCS7 hydronic heat | Its local HTTP API | Setpoint, fan, heat sources, hot water, engine pre-heat, burner diagnostics, fuel use |
| Espar/Eberspächer Airtronic S2 (EasyStart Pro bus) | CAN, 500 kbit/s | Heat, fan, run timer, live diagnostics, fault memory. AetherRV *replaces* the EasyStart Pro. |
| Carling/Maretron CLMD16 + CKM12 keypads | NMEA 2000 (CAN HAT, or the Cerbo's VE.Can as a backup) | Switch and dim loads, per-channel current, wired inputs |
| Starlink | Local gRPC | Speeds, obstruction sky map, heating, sleep schedule, updates |
| Peplink router | Its router API (read-only user) | WAN status and priority, cellular signal, GPS position |

## How it fits together

- **Arch Linux ARM** runs on the Pi. AetherRV lives in `/opt/aetherrv-rs`, its configuration in
  `/etc/aetherrv/aetherrv.toml`, secrets in `/etc/aetherrv/secrets.toml`, and its history in `/var/lib/aetherrv`.
- **One small program per device** (`aetherrv-espar`, `aetherrv-n2k`, `aetherrv-victron`, `aetherrv-rixens`, …),
  each its own systemd unit, plus a few core services (alerts, history, settings, the low-battery guard). They
  share state over a local MQTT broker (`aetherrv-bus`, Mosquitto on a Unix socket, nothing on the network), so a
  crash or restart of one never takes the others down. `aetherrv.target` starts them all.
- **`aetherrv-gateway`** serves the screen (a React app) and its WebSocket on port 8080.
- **`aetherrv-panel.service`** logs a `kiosk` user into a sway session on the screen and runs the UI full screen.
- **`aetherrv-admind.socket`** is the only part that runs as root: a small helper with a fixed list of actions
  (Wi-Fi, updates, reboot, CAN recovery) that the Settings page uses.
- **CAN ports** get stable names (`hat0`, `hat1`, `fd0`, `fd1`) from systemd-networkd and start **listen-only**:
  the Pi can't send a single frame until you change that, port by port.
- **You deploy from your computer:** `packaging/push.sh <pi>` builds the UI, copies the source to the Pi, builds
  the programs natively there and runs the installer.

## 0. Ground rules for your AI

Paste this into your AI assistant first:

```text
You're helping me build an AetherRV console (Raspberry Pi 5, Arch Linux ARM) following
docs/BUILD-GUIDE.md in the AetherRV repository, which I've given you. Explain each step in one or two
plain sentences before you run it, and tell me what success looks like. Rules for the whole job:
- Before any command that writes to a disk, show me the disks (lsblk with model, size and transport)
  and wait for me to confirm which one to use. Never write to any other disk.
- When a command needs a password or token, give me the command to run myself. Never ask me to paste
  a password or token into this chat, and never write one into a file or command line yourself.
- Never send anything to a device in my van (CAN frames, HTTP writes, MQTT writes) unless I've asked
  for that specific action. Reading is fine.
- Keep every CAN port listen-only until I say otherwise.
- If something doesn't match the guide, stop and ask me.
```

## 1. Put the hardware together

**What's happening:** you assemble the Pi with its storage, CAN boards and cooler, and connect the screen and
power. Do this with everything unplugged.

1. Fit the **active cooler** to the Pi 5.
2. Fit the **NVMe adapter** and SSD (if you use one), following its instructions. The reference build uses
   the Waveshare NVMe + PoE HAT; whatever you use, it must leave the 40-pin header free for the CAN boards or
   pass it through.
3. Stack the **2-CH CAN HAT+** on the 40-pin header, then the **CAN FD HAT** on top if you have one. Use the
   standoffs that come with them so nothing touches the cooler.
4. On each CAN board, set the **120 Ω termination jumpers/switches to OFF** for now. Step 8 explains when to
   turn one on.
5. If you fit the CAN FD HAT, set its **VIO** jumper to **3.3 V**.
6. Connect the screen: micro-HDMI port **0** (next to the USB-C power) to the screen's HDMI, and a USB cable
   from the screen's touch port to the Pi.
7. Wire the **5 V converter** to a **fused** 12 V circuit (a 3–5 A fuse is plenty) and plug its USB-C into the
   Pi. Don't power it up yet.

**Check:** nothing is loose, the HAT pins are fully seated, and every termination switch is off.

> **Prompt:** "I've assembled the Pi 5 with <my NVMe adapter or 'no NVMe'>, the CAN HAT+ <and the CAN FD HAT>.
> Walk me through anything I should double-check before first power-up: jumpers, termination, VIO and
> stacking clearance."

## 2. Install Arch Linux ARM onto the drive

**What's happening:** you put Arch Linux ARM on the NVMe (or SD card) from your Linux computer: two partitions,
a FAT32 boot partition and an ext4 system partition, then the Arch Linux ARM image unpacked onto them. The
reference build runs a custom-built kernel, but you don't need one: Arch Linux ARM's own Raspberry Pi kernel
has the CAN drivers and overlays this guide uses.

1. Put the NVMe in the USB enclosure (or the SD card in the reader) and plug it into your Linux computer.
2. Find it. **Get this right: everything on it will be erased.**

```sh
lsblk -o NAME,SIZE,MODEL,TRAN
```

3. Partition, format and mount it (as root; `DISK` is the one you confirmed):

```sh
DISK=/dev/sdX
sgdisk --zap-all "$DISK"
sgdisk -n1:0:+512M -t1:0700 -c1:boot -n2:0:0 -t2:8300 -c2:root "$DISK"
P1=${DISK}1; P2=${DISK}2              # if the disk name ends in a digit: ${DISK}p1 / ${DISK}p2
mkfs.vfat -F32 -n BOOT "$P1"
mkfs.ext4 -L root "$P2"
mkdir -p /mnt/alarm && mount "$P2" /mnt/alarm
mkdir -p /mnt/alarm/boot && mount "$P1" /mnt/alarm/boot
```

4. Download and unpack the Arch Linux ARM aarch64 image (as root, so file owners are kept):

```sh
curl -LO http://os.archlinuxarm.org/os/ArchLinuxARM-rpi-aarch64-latest.tar.gz
bsdtar -xpf ArchLinuxARM-rpi-aarch64-latest.tar.gz -C /mnt/alarm
sync
```

5. Point the system at its own partitions, then unmount:

```sh
cat /mnt/alarm/etc/fstab                         # should mount /boot; fix the device if it says mmcblk
blkid "$P2"                                       # note the PARTUUID
umount -R /mnt/alarm
```

Arch Linux ARM's image assumes an SD card (`/dev/mmcblk0p1`). For an NVMe drive, change `/boot`'s line in
`/etc/fstab` to `/dev/nvme0n1p1`, and once the Raspberry Pi kernel is installed (step 3), make sure
`/boot/cmdline.txt` says `root=PARTUUID=<the root PARTUUID>`.

6. **NVMe only:** the Pi 5 must be told to boot from NVMe. The easiest way is once, from an SD card: in
   Raspberry Pi Imager choose *Misc utility images → Bootloader (Pi 5) → NVMe/USB Boot*, write it to any SD
   card, boot the Pi from it until the screen shows the green success pattern, then remove the card. Also
   add `dtparam=pciex1` to `/boot/config.txt` (step 4) so the NVMe adapter is enabled.

**Check:** the drive has two partitions, `boot` and `root`, and `/etc/fstab` names the right device.

> **Prompt:** "Do step 2 of the build guide. Show me lsblk and wait for me to confirm the drive. I'm
> installing to <NVMe in a USB enclosure / an SD card>. Fix fstab and cmdline.txt for that drive."

## 3. First boot and base system

**What's happening:** the Pi starts Arch Linux ARM for the first time. You swap in the Raspberry Pi kernel,
update everything, install the packages AetherRV uses, and lock SSH down to your key.

1. Fit the drive, connect Ethernet, and power up. Log in over SSH as `alarm` (password `alarm`); find the
   Pi's address in your router's client list.
2. Become root (`su`, password `root`) and set up the package keys and the Raspberry Pi kernel:

```sh
pacman-key --init && pacman-key --populate archlinuxarm
pacman -Syu
pacman -R linux-aarch64 uboot-raspberrypi        # the generic kernel and U-Boot
pacman -S linux-rpi raspberrypi-bootloader firmware-raspberrypi
```

3. Install what AetherRV needs:

```sh
pacman -S --needed sudo openssh rsync git base-devel clang rustup mosquitto \
  sway chromium noto-fonts wlr-randr grim \
  iwd bluez bluez-utils \
  plymouth tailscale
rustup default stable                            # as the admin user from step 4, not root
```

`base-devel`, `clang` and `rustup` build AetherRV on the Pi; `mosquitto` is its internal message bus; `sway` and
`chromium` run the screen (the UI runtime: see docs/UI-RUNTIME-BENCHMARK.md); `iwd` is Wi-Fi; `bluez` is
Bluetooth for the batteries; `plymouth` is the boot splash and `tailscale` is remote access (both optional).
On your computer you need `git`, `rsync` and Node.js (`npm`) to build the screen's app.

4. Your admin account, SSH key only, and a name:

```sh
useradd -m -G wheel <you> && passwd <you>          # type the password yourself
echo '%wheel ALL=(ALL:ALL) ALL' > /etc/sudoers.d/10-wheel && chmod 440 /etc/sudoers.d/10-wheel
install -d -m700 -o <you> -g <you> /home/<you>/.ssh
echo 'ssh-ed25519 AAAA... you@computer' > /home/<you>/.ssh/authorized_keys   # your PUBLIC key
chown <you>: /home/<you>/.ssh/authorized_keys && chmod 600 /home/<you>/.ssh/authorized_keys
printf 'PermitRootLogin no\nPasswordAuthentication no\n' > /etc/ssh/sshd_config.d/10-keyonly.conf
hostnamectl set-hostname aetherrv
userdel -r alarm && passwd -l root
systemctl enable bluetooth
reboot
```

5. On your computer, add an SSH alias so later steps can use `ssh aetherrv`:

```text
Host aetherrv
    HostName <the Pi's address>
    User <you>
    IdentityFile ~/.ssh/<your key>
```

**Check:** `ssh aetherrv uname -r` prints a kernel version ending in `-rpi` (or similar), and
`ssh aetherrv sudo true` works.

> **Prompt:** "Do step 3 of the build guide over SSH. Hand me every passwd command to run myself. Make my
> SSH key the only way in, and confirm the Raspberry Pi kernel is the one booting."

## 4. Turn on the CAN ports

**What's happening:** the CAN boards don't announce themselves, so you tell the Pi they're there
(device-tree overlays in `/boot/config.txt`). Then systemd-networkd gives each port a stable name and starts it
**listen-only**, so it can hear the bus but can't send anything.

1. Add to the end of `/boot/config.txt` (keep a backup first: `cp /boot/config.txt /boot/config.txt.bak`):

```ini
# NVMe adapter (only if you use one)
dtparam=pciex1

# Waveshare 2-CH CAN HAT+ (MCP2515, 16 MHz)
dtparam=spi=on
dtoverlay=i2c0
dtoverlay=spi1-3cs
dtoverlay=mcp2515,spi1-1,oscillator=16000000,interrupt=22
dtoverlay=mcp2515,spi1-2,oscillator=16000000,interrupt=13

# Waveshare 2-CH CAN FD HAT, stacked (only if fitted)
dtoverlay=mcp251xfd,spi0-0,interrupt=25
dtoverlay=mcp251xfd,spi1-0,interrupt=24
```

These are the pin choices from Waveshare's wikis for these two boards stacked together. If you use different
boards, check their wikis: no two ports may share a chip-select or interrupt pin.

2. Stable names. The kernel numbers CAN ports in whatever order they start, so pin each port by its SPI
   location. Create these files in `/etc/systemd/network/`:

```ini
# 10-hat0.link   (CAN HAT+ port 0)
[Match]
Path=platform-1f00054000.spi-cs-1
Driver=mcp251x
[Link]
Name=hat0
```

```ini
# 10-hat1.link   (CAN HAT+ port 1)
[Match]
Path=platform-1f00054000.spi-cs-2
Driver=mcp251x
[Link]
Name=hat1
```

```ini
# 10-fd0.link and 10-fd1.link   (CAN FD HAT, only if fitted)
[Match]
Path=platform-1f00050000.spi-cs-0          # fd1: platform-1f00054000.spi-cs-0
Driver=mcp251xfd
[Link]
Name=fd0                                    # fd1
```

3. One `.network` file per port, **listen-only**, at the bus's bitrate (NMEA 2000 is 250000; the Espar's bus
   is 500000). For example `30-hat0.network`:

```ini
[Match]
Name=hat0
[Link]
RequiredForOnline=no
[CAN]
BitRate=250000
ListenOnly=yes
RestartSec=1s
```

Make `30-hat1.network` the same with `Name=hat1` (and `BitRate=500000` if it will be the Espar's port), and
likewise for `fd0`/`fd1`.

4. `sudo systemctl enable systemd-networkd && sudo reboot`.

**Check:** `ip -details link show hat0` shows `state UP`, `LISTEN-ONLY` and `ERROR-ACTIVE` (or `ERROR-PASSIVE`
with nothing wired yet), and `dmesg | grep -i mcp25` shows each chip initialized.

> **Prompt:** "Do step 4 of the build guide for <the CAN HAT+ only / the CAN HAT+ and CAN FD HAT>. Back up
> config.txt first, show me the diff, and keep every port listen-only. After the reboot, show me each port's
> state."

## 5. Install AetherRV

**What's happening:** you download AetherRV on your computer and push it to the Pi. `push.sh` builds the
screen's app, copies the source, and builds the programs on the Pi itself (about 6–12 minutes the first time).
The installer creates the `aetherrv` and `kiosk` users, the services, the screen session and a starter
configuration, then starts everything. It never touches `/boot` or existing configuration.

On your computer:

```sh
git clone -b rust https://github.com/aethersdr/AetherRV.git && cd AetherRV
(cd ui && npm ci)
packaging/push.sh aetherrv --system
```

**Check:** the last line says `installed to /opt/aetherrv-rs (…) and started: active / panel active`, and the
screen shows AetherRV. With nothing configured yet most cards are empty or say "not connected"; that's expected.
`ssh aetherrv sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl health` lists every module and its heartbeat.

**Kernel updates:** the installer holds `linux-rpi` and the boot firmware packages (`IgnorePkg` in
`/etc/pacman.conf`) because the reference build boots a custom kernel. On the stock Raspberry Pi kernel you
can remove those three names from `IgnorePkg` so system updates (Settings → Updates) include the kernel.

> **Prompt:** "Do step 5 of the build guide: clone AetherRV (branch rust), install the UI's packages and run
> packaging/push.sh with my SSH alias and --system. Then show me aetherrv-ctl health and the last 30 lines of
> the AetherRV logs (journalctl -u 'aetherrv-*')."

## 6. Configure your equipment

**What's happening:** you tell AetherRV what's in your van. Everything lives in one file on the Pi,
`/etc/aetherrv/aetherrv.toml`, which starts as a copy of `config/aetherrv.example.toml`. Each device has a
`[device.<name>]` section; comment out what you don't have. Secrets (passwords, tokens) never go in this file:
the tools below write them to `/etc/aetherrv/secrets.toml`, readable only by root and AetherRV.

| Equipment | What to set |
|---|---|
| Victron GX | `[device.victron]`: `host` = the GX's address, `enabled = true`. Then pair: `sudo /opt/aetherrv-rs/bin/aetherrv-setup victron-pair <gx-address>` and enable MQTT pairing on the GX (Settings → Integrations → MQTT) while it waits. |
| Ruuvi / temperature sensors on the GX | `[device.victron]` `cabin_temp`, `outside_temp`, `fridge_temp`, `freezer_temp`: the sensors' names on the GX. |
| Rixens MCS7 | `[device.rixens_http]`: `host` = the MCS7's address on your LAN, `enabled = true`. |
| Starlink | `[device.starlink]`: `enabled = true`; `target` is the dish's gRPC address (usually `192.168.100.1:9200`, reachable through the router). |
| Peplink | `[device.peplink]`: `host` = the router, `enabled = true`. Create the read-only API user with `tools/peplink_install_readonly_user.sh` and `tools/peplink_create_api_client.sh` (run from your computer; they prompt for passwords). |
| Batteries over Bluetooth | `[device.epoch_ble]` (commented out in the sample) with one `[[device.epoch_ble.battery]]` per battery: its Bluetooth address and a name. `bluetoothctl scan on` lists them. |
| NMEA 2000 switching | `[device.n2k]`: the bus, then one `[[device.n2k.switch]]` per channel (name, icon, group, Home position). |
| Espar heater | `[device.espar]`: the bus (`hat1`). Leave `tx_enabled = false` until step 8. |
| TomTom traffic | `tools/set_tomtom_key.sh`, from your computer (prompts for the key on the Pi). |
| Remote control | `[web] control_subnets` / `control_tailnet`: who besides the touchscreen may control. |
| Low-battery shutdown | `[power] auto_shutdown = true`: the Pi powers itself off cleanly when the house bank stays at or below `shutdown_soc` (5 %) for 2 minutes while not charging, before the battery BMS cuts it off. A Pi 5 then stays off until its power is cycled or its button is pressed. This replaces a UPS HAT. Earlier, at `heater_off_soc` (10 %), heat is locked out: both heaters are turned off and kept off, so they're never burning (or still in their after-run) when the BMS cuts power, which can seriously damage them. Before the 5 % power-off, AetherRV waits for the after-runs to finish. |

After each change: `sudo systemctl restart aetherrv.target` (or just the one service, e.g. `aetherrv-victron`).

**Remote access (optional):** `sudo systemctl enable --now tailscaled && sudo tailscale up`, then sign in with
the link it prints (or use Settings → Tailscale on the screen).

**Wi-Fi (optional):** Settings → Wi-Fi on the screen.

**Boot splash (optional, changes `/boot`):** `sudo /opt/aetherrv-rs/src/tools/install_boot_splash.sh`.

**Check:** each device you configured shows live values within a minute, and the Systems page lists its
messages.

> **Prompt:** "Do step 6 of the build guide. My van has: <list your equipment and their addresses>. Edit
> /etc/aetherrv/aetherrv.toml on the Pi for exactly those (back it up first), comment out the rest, and give
> me any pairing or password commands to run myself. Restart AetherRV and show me which devices are live."

## 7. Read first, control later

**What's happening:** every device starts **read-only**. AetherRV's `TxGate` refuses every write until that
device's `tx_enabled = true` is set in `/etc/aetherrv/aetherrv.toml`, and CAN ports stay listen-only at the
kernel level until you change their `.network` file. Turn control on one device at a time:

1. Watch the device's values for a while and confirm they match the device's own display.
2. Set `tx_enabled = true` for that device only (and, for a CAN device, `ListenOnly=no` on its port), restart.
3. Try one harmless command from the touchscreen (a spare switch channel, a fan speed) and confirm the device
   changed and AetherRV says it verified the change.

Every command reads fresh state first, sends once, verifies, never retries, and is logged.

> **Prompt:** "I want to enable control of <device>. Show me what AetherRV has read from it over the last few
> minutes and whether it matches what I see on the device. Then enable tx for that device only, restart, and
> tell me the single safest command to test it with from the touchscreen. Don't send anything yourself."

## 8. Wire the CAN buses

**NMEA 2000 (to `hat0`).** Cut a drop cable and connect it to the HAT's port 0 terminal block. On standard N2K
cable:

| N2K wire | CAN HAT |
|---|---|
| White (CAN H) | **H** |
| Blue (CAN L) | **L** |
| Black (ground/shield return) | **G** |
| Red (+12 V) | **Not connected.** Insulate it. |
| Bare drain | Not connected. |

The backbone already has its two terminators, so **hat0's 120 Ω stays off**. Plug the drop into a free tee.
At 250 kbit/s in listen-only mode you should see the Systems page fill with N2K messages within seconds.

**Espar Airtronic S2 (to `hat1`, 500 kbit/s).** On the EasyStart Pro harness (from Eberspächer's circuit
diagram), the CAN pair is the two thin wires, **blue/black** and **blue/red** (often looks violet), with
**brown** as ground; the red wire is +12 V and stays unconnected. Try blue/black on **H** and blue/red on **L**;
in listen-only mode a swap is harmless (you'll see errors and no messages; swap them).

Tap the harness where its plug-in **terminator** sits and **leave the terminator in place**, with **hat1's 120 Ω
off**. The heater's bus then keeps its termination even if the Pi is unplugged.

> [!WARNING]
> The heater expects exactly **one** controller. Once AetherRV controls the Espar (`tx_enabled = true`),
> unplug the EasyStart Pro and keep it as a spare; never connect both. If AetherRV stops (Pi off, crash,
> cable pulled), the heater shuts itself down within about 15 s and runs its normal cool-down.
> `docs/espar-control-design.md` has the protocol and the bench test plan; follow it before relying on it.

> **Prompt:** "I've wired <N2K to hat0 / the Espar to hat1> as in step 8 of the build guide. Check the port
> is up and listen-only, show me the message IDs it's receiving and their rates, and tell me if the wiring
> looks right (H/L swapped, termination, bitrate). Don't send anything."

## 9. Looking after it

- **Updates:** Settings → Updates installs Arch updates; `packaging/push.sh aetherrv` from your computer builds
  and installs a new AetherRV version (it doesn't restart anything; `sudo systemctl restart aetherrv.target`
  does). The Espar service re-attaches to a running heater after a restart without stopping it, but until the
  heater's own controller-loss timeout has been measured on your van, update with the heater off.
- **Back up** `/etc/aetherrv/` and `/etc/systemd/network/` after you change them.
- **History** is in `/var/lib/aetherrv/history.sqlite3` (30 days by default). Don't create files there as
  root; use `sudo -u aetherrv`.
- **Logs:** `journalctl -u 'aetherrv-*' -f` (one service: `journalctl -u aetherrv-espar -f`).
- **Module health:** `sudo -u aetherrv /opt/aetherrv-rs/bin/aetherrv-ctl health`; the Systems page shows the same.

## If something goes wrong

| Symptom | Likely cause |
|---|---|
| A CAN port is missing after a reboot, or its log says the chip "didn't enter config mode" | The MCP2515 occasionally fails to start. AetherRV rebinds the driver by itself after about 30 s; if not, reboot. |
| CAN port up but no messages | H/L swapped, wrong bitrate, or missing termination at one end of the bus. |
| Screen black but SSH works | `systemctl status aetherrv-panel` and `journalctl -u aetherrv-panel`. |
| One card stuck or "module down" | `aetherrv-ctl health`; `journalctl -u aetherrv-<module>`; `sudo systemctl restart aetherrv-<module>` (the others keep running). |
| A device shows "not connected" | Its address in `aetherrv.toml`, or the network between them (`ping` from the Pi). |
| Controls are greyed out | That device's `tx_enabled` is off, or you're on a browser that isn't allowed to control (`[web] control_subnets`). |

## Make it yours: prompts for your AI

AetherRV is organized so that each piece of equipment is one **service** in `services/` (with its protocol in
`crates/proto-*`). Services only publish values onto the bus (`climate.*`, `victron.*`, `switches.*`…; see
docs/ARCHITECTURE.md §4) and accept a small, checked set of commands, and the screen in `ui/` draws whatever is in
that state. Adapting to different equipment usually means writing one new service (docs/SERVICE-GUIDE.md) and
pointing the existing cards at its values. Give your assistant the repository and one of these prompts.

**Start here, whatever you have:**

> "Read the AetherRV repository (branch rust): README.md, docs/BUILD-GUIDE.md, docs/ARCHITECTURE.md, services/
> and config/aetherrv.example.toml. My van has <list everything: battery system, charger/inverter, heater,
> switching, router, internet, sensors>. Tell me which of these AetherRV already supports, which need
> configuration only, and which need a new service, with the effort for each. Don't change anything yet."

**Different battery monitor or BMS:**

> "My batteries are <brand/model> and report over <Bluetooth / CAN / a Victron or other monitor>. Find out
> how their BMS protocol works (look for open-source projects such as aiobmsble that support it), then add
> support to AetherRV the way services/batteries-ble does: read-only, publishing the same batteries.ble.*
> fields so the Energy page's battery bank works unchanged. Write tests from captured sample data."

**No Victron (Renogy, EPEver, Battle Born, Sterling, a plain shunt…):**

> "I don't have a Victron GX. My power system is <devices and how they connect: VE.Direct, Modbus, RS-485,
> Bluetooth, CAN>. Design a service that publishes the same victron.* power-flow values the Energy and Home
> pages use (battery voltage/current/SoC, solar, shore, alternator, loads), read-only first. Show me the plan
> before writing code."

**Different heater (Webasto, Truma, Dometic, a Chinese diesel heater, another Espar model):**

> "My heater is <make/model> with a <controller> on <W-bus / LIN / CAN / serial>. Research its protocol
> (open-source projects, service manuals), then add a read-only service that fills a heater card on the
> Climate page the way services/espar does. Don't add any control until I've reviewed the protocol and
> we've planned a supervised bench test like docs/espar-control-design.md."

**Different switching (RV-C, a different N2K load controller, relays on GPIO, Shelly, Victron GX relays):**

> "My loads are switched by <system>. Show me how AetherRV's switching works (services/n2k,
> its switching.rs, the Switches page) and plan an adapter that publishes the same switches.list entries and
> accepts the same set/press commands, with the same safety rules: fresh read, send once, verify, never
> retry, keep any interlocks in the device itself."

**RV-C instead of NMEA 2000:**

> "My van uses RV-C (CAN, 250 kbit/s). Using the CAN port setup in step 4, capture the bus listen-only for a
> few minutes while I operate things, decode it against the RV-C specification, and tell me which devices
> are on it and what AetherRV could show. Read-only."

**Different router or internet (GL.iNet, Cradlepoint, Winegard, no Starlink, a phone hotspot):**

> "Instead of Peplink/Starlink I have <devices>. Add read-only services that fill the Network page's WAN and
> signal cards from their APIs, and turn off the Peplink and Starlink cards. No router control without my
> approval."

**Different screen:**

> "My screen is <model>, <resolution>, <HDMI or DSI>. Update packaging/panel/sway.conf for its output name and
> mode, check the touch input maps to it, and tell me which pages in ui/ assume 1920×1200 and what to change
> so everything fits without scrolling. Show me screenshots of every page afterwards."

**Raspberry Pi OS (or another Linux) instead of Arch:**

> "I want to run AetherRV on <Raspberry Pi OS Bookworm / Debian / Ubuntu>. Go through packaging/install.sh,
> packaging/system-setup.sh, services/admind and the build guide, list everything that's Arch-specific (pacman, package names, iwd,
> IgnorePkg, paths), and adapt them for my distribution with the same users, services and safety model."

**New page or card:**

> "Add <what you want to see> to the <page> page. Use the existing card styles in ui/src/styles, keep it
> readable at arm's length on a 10.1-inch touchscreen, and show me a screenshot from the Pi before
> committing."

**Before you enable control of anything new:**

> "Before I enable control of <device> in AetherRV: walk through the command path in its service, show me
> exactly which messages it can send and with what checks, what happens if the Pi crashes mid-command, and
> what the device does if AetherRV disappears. Then plan the smallest supervised test."
