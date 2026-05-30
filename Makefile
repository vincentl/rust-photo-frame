.PHONY: kiosk-diagnostics diag-kiosk showcase

kiosk-diagnostics:
	@./setup/system/tools/diagnostics.sh

diag-kiosk: kiosk-diagnostics

# Run the showcase tour locally (see demo/README.md for photo setup).
# Edit demo/showcase.yaml → photo-library-path to point at demo/photos.
showcase:
	cargo run -p photoframe -- demo/showcase.yaml
