SHELL := /bin/sh
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)

.PHONY: smoke daily wifi-recovery collect docs-links clean

smoke:
	@cd $(ROOT)/tests && ./run_smoke.sh

daily:
	@cd $(ROOT)/tests && ./run_daily.sh

wifi-recovery:
	@cd $(ROOT)/tests && ./run_wifi_recovery.sh

collect:
	@cd $(ROOT)/tests && ./collect_logs.sh

docs-links:
	@cd $(ROOT)/tests && ./check_markdown_links.sh

clean:
	@rm -rf $(ROOT)/artifacts/* $(ROOT)/tests/tmp 2>/dev/null || true
	@mkdir -p $(ROOT)/artifacts
	@echo "Artifacts directory reset"
