# First Boot: What to Expect

You've finished the install. The Pi is booting. Here's exactly what should happen, what's normal, and what means something is wrong.

---

## The normal sequence

When everything is working, the display goes through these states in order:

**1. Blank display** (a few seconds)
The kiosk session is starting — greetd launches Sway, which sets up the Wayland compositor. The display is blank during this handoff.

**2. Greeting screen** (~16 seconds)
A styled card appears: "Warming up your photo memories…" This is the app loading, scanning the photo library, and pre-decoding the first images. The greeting stays up for at least `greeting-screen.duration-seconds` from `config.yaml` (default: 16 seconds).

**3. Display goes dark — this is normal**
After the greeting, the frame enters sleep state. The display goes blank. **This does not mean something is broken.**

The frame is running. The GPU is idle. It's waiting for either a wake command or the start of a scheduled awake window. Without a configured `awake-schedule`, you need to send a wake command manually.

**4. Photos begin cycling** (after you wake the frame)
Once awake, the frame starts displaying photos from the library with configured transitions and matting.

---

## The sleep state

"Sleep" means:
- The `photoframe` process is running
- The kiosk Wayland session is active
- The GPU is not rendering (no photo cycling)
- The display may blank or go into DPMS power-saving mode depending on your monitor and `buttond.screen` configuration

The frame shows a sleep card briefly ("Tucking in for a nap…") during the transition, then goes dark.

To confirm the frame is running and just asleep:

```bash
sudo systemctl status greetd.service
```

It should say `active (running)`. Then check the socket:

```bash
sudo ls -l /run/photoframe/control.sock
```

If both are present, the frame is healthy and asleep.

---

## How to wake the frame

Send a JSON command over the Unix control socket:

```bash
echo '{"command":"set-state","state":"awake"}' \
  | sudo -u kiosk socat - UNIX-CONNECT:/run/photoframe/control.sock
```

Breaking that command down:
- `echo '...'` — produces the JSON wake command
- `sudo -u kiosk` — runs as the `kiosk` user, who owns the socket
- `socat - UNIX-CONNECT:/run/photoframe/control.sock` — connects to the socket and sends stdin

After a second or two you should see photos begin cycling on the display.

**For always-on operation** (frame never sleeps automatically): leave the `awake-schedule` block commented out in `/etc/photoframe/config.yaml`. Without a schedule, `buttond` keeps the frame awake at all times. You can still send sleep/wake commands manually.

---

## If the greeting never appears

The kiosk session didn't start. Check:

```bash
sudo systemctl status greetd.service
sudo journalctl -u greetd.service -b --no-pager | tail -30
```

Common causes:
- **`greetd` failed to start** — look for errors in the journal; often a missing config file or permission issue. Re-run `./setup/application/deploy.sh` to re-provision.
- **Display not connected** — greetd may not fully launch without an HDMI signal. Connect the monitor before booting.
- **Install didn't complete** — run `./setup/tools/verify.sh` to identify what's missing.

---

## If photos don't appear after waking

**Check that photos are in the right place:**

```bash
find /var/lib/photoframe/photos -type f | head -20
```

If that returns nothing, the library is empty. Add photos following [docs/installation.md Step 4](installation.md#step-4--add-your-photos).

**Check for permission errors in logs:**

```bash
sudo journalctl -t photoframe -n 50 --no-pager
```

Look for lines containing `error` or `permission`. If photos were copied as root, fix ownership:

```bash
sudo chown -R kiosk:kiosk /var/lib/photoframe/photos/
```

**Supported formats:** JPEG and PNG. Other formats are skipped silently.

---

## What healthy logs look like

Run this while the frame is awake and cycling:

```bash
sudo journalctl -t photoframe -f
```

Healthy output:

```
photoframe: scanning /var/lib/photoframe/photos
photoframe: found 42 photos
photoframe: loaded sunset.jpg (3840x2160)
photoframe: displaying photo 3 of 42
photoframe: transition fade 450ms
photoframe: loaded mountains.jpg (4000x3000)
```

If you see errors about a specific photo file (decode errors, unsupported format), the frame skips that file and marks it invalid — it won't try again until restarted.

---

## Quick state reference

| Display state | What it means |
| --- | --- |
| Blank (boot) | Session starting, normal |
| Greeting card | App loading and scanning library |
| Dark / blank | Frame asleep — send wake command |
| Sleep card briefly, then dark | Entering sleep state |
| Photos cycling | Frame awake and healthy |
| Same photo stuck | Transition in progress or only one photo in library |

---

## Next steps

- **Daily commands:** [docs/quick-reference.md](quick-reference.md)
- **Configure schedule or transitions:** [docs/configuration.md](configuration.md)
- **Something isn't right:** [docs/troubleshooting.md](troubleshooting.md)
