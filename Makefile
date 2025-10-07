.PHONY: kiosk-diagnostics diag-kiosk

kiosk-diagnostics:
@./setup/kiosk/diagnostics.sh

diag-kiosk: kiosk-diagnostics
