#!/usr/bin/env python3
"""Monitor a physical button to control display power and initiate shutdown."""

import logging
import os
import signal
import subprocess
import sys
import time

from gpiozero import Button
from gpiozero.exc import BadPinFactory

BUTTON_GPIO = int(os.getenv("BUTTON_GPIO", "17"))
SHUTDOWN_HOLD_SECONDS = float(os.getenv("SHUTDOWN_HOLD_SECONDS", "5"))
DISPLAY_OFF_HOLD_SECONDS = float(os.getenv("DISPLAY_OFF_HOLD_SECONDS", "0.2"))
LOG_LEVEL = os.getenv("LOG_LEVEL", "INFO").upper()

logging.basicConfig(
    level=getattr(logging, LOG_LEVEL, logging.INFO),
    format="%(asctime)s [button-monitor] %(levelname)s: %(message)s",
)

running = True

def handle_exit(signum, frame):
    global running
    logging.info("Received signal %s, shutting down monitor", signum)
    running = False


def toggle_display():
    logging.info("Toggling display power state")
    try:
        current = subprocess.check_output(["vcgencmd", "display_power"]).decode().strip()
        next_state = "1" if "=0" in current else "0"
        subprocess.check_call(["vcgencmd", "display_power", next_state])
        logging.info("Display power set to %s", next_state)
    except Exception as exc:  # noqa: BLE001 - broad to ensure we log hardware issues
        logging.exception("Failed to toggle display power: %s", exc)


def initiate_shutdown():
    logging.warning("Button held for %.1f seconds. Initiating shutdown.", SHUTDOWN_HOLD_SECONDS)
    try:
        subprocess.check_call(["/sbin/shutdown", "-h", "now"])
    except Exception as exc:  # noqa: BLE001
        logging.exception("Failed to trigger shutdown: %s", exc)


def main() -> int:
    signal.signal(signal.SIGTERM, handle_exit)
    signal.signal(signal.SIGINT, handle_exit)

    try:
        button = Button(BUTTON_GPIO, pull_up=True, bounce_time=0.05)
    except BadPinFactory as exc:
        logging.error("Failed to initialize GPIO pin factory: %s", exc)
        logging.error(
            "Ensure the lgpio package is installed and the service has access to /dev/gpiomem"
        )
        return 1
    except Exception as exc:  # noqa: BLE001 - log unexpected hardware init errors
        logging.exception("Unexpected failure initializing button: %s", exc)
        return 1
    logging.info("Monitoring button on GPIO %s", BUTTON_GPIO)

    while running:
        if button.is_pressed:
            pressed_at = time.monotonic()
            while button.is_pressed:
                if time.monotonic() - pressed_at >= SHUTDOWN_HOLD_SECONDS:
                    initiate_shutdown()
                    return 0
                time.sleep(0.05)
            held_seconds = time.monotonic() - pressed_at
            if held_seconds >= DISPLAY_OFF_HOLD_SECONDS:
                toggle_display()
        time.sleep(0.1)

    logging.info("Button monitor exiting cleanly")
    return 0


if __name__ == "__main__":
    sys.exit(main())
