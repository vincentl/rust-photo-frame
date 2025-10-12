.PHONY: kiosk-diagnostics diag-kiosk

kiosk-diagnostics:
@./setup/system/tools/diagnostics.sh

diag-kiosk: kiosk-diagnostics
