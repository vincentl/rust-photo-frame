.PHONY: kiosk-diagnostics diag-kiosk showcase

kiosk-diagnostics:
	@./setup/system/tools/diagnostics.sh

diag-kiosk: kiosk-diagnostics

# Run the showcase tour locally (see showcase/README.md for setup).
# showcase.yaml defaults to the Pi media path; for a local run, copy it and
# point photo-library-path at a local folder first.
showcase:
	cargo run -p photoframe -- showcase/showcase.yaml
