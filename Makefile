.PHONY: kiosk-diagnostics diag-kiosk

kiosk-diagnostics:
@./setup/bootstrap/tools/diagnostics.sh

diag-kiosk: kiosk-diagnostics
