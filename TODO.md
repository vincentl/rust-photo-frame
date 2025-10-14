# To Do

## Work On

[ ] initial aspect ratio
[ ] iris




[ ] Validate Wifi Manager

[ ] New install

- photos/local and photos/cloud not found

[ ] Hesitation on wipe transition
[ ] Test button 1 or 2 click events
[ ] Cloud sync
[ ] Tailscale

[ ] Sleep schedule
[ ] Validate log rotation
[ ] Test add new photo behavior
[ ] Small set of default images - license?

[X] Test fan performance
[X] Review boot firmware settings to old commit
[X] Monitor Wakeup is not working
[X] Sleep blacken screen again?
[X] Provision fresh pi
[X] Kiosk mode
[X] remove ./setup/migrate/legacy-cleanup.sh`
[X] Flashing??
[X] move fabrication.md from docs to maker? maybe move maker into docs?
[X] adit all the doc files and remove extraneous

```bash
sudo -u frame env XDG_RUNTIME_DIR=/run/user/$(id -u frame) WAYLAND_DISPLAY=wayland-0 /opt/photo-frame/bin/rust-photo-frame /opt/photo-frame/etc/config.yaml
sudo env XDG_RUNTIME_DIR=/run/user/$(id -u kiosk) wlr-randr --json

watch -n 1 'vcgencmd measure_temp; cat /sys/class/thermal/cooling_device0/cur_state 2>/dev/null'
sudo apt install stress-ng -y
stress-ng --cpu 0 --io 2 --vm 2 --vm-bytes 512M --timeout 90s



```

### Physical

[ ] Start on frame, cleat, pi case makes
[ ] Setup tailscale
[ ] Setup pcloud for testing

## Major Steps

[ ] Parts list
[ ] Dell Monitor
[ ] Pi 5 at least 4 GiB
[ ] microSD 128GiB
[ ] Cables - 4K micro to full HDMI, USB-C
[ ] Power hat
[ ] Cooler
[ ] Momentary button
[ ] GPIO pins
[ ] Riser parts
[ ] M4 bolts
[ ] Felt
[ ] Black acrylic
[ ] Plywood 3/4 and 1/2
[ ] Oak boards
[ ] Black spray paints
[ ] Wood stain
[ ] Wood glue
[ ] Pocket jig and screws
[ ] Soldier Iron and soldier and flux
[ ] Make list
[ ] Pi case
[ ] Cleat
[ ] Frame
[ ] Prep Pi Hardware
[ ] describe sequence & case
[ ] take pictures
[ ] Frame
[ ] describe design
[ ] pictures
[ ] Cleat
[ ] describe design
[ ] pictures
[ ] Software
[ ] system update
[ ] packages like rust
[ ] Main app
[ ] cloud syncing
[ ] wifi watcher
[ ] font
[ ] tailscale
