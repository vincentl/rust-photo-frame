SHELL := /bin/sh
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)

.PHONY: smoke daily collect clean

smoke:
@cd $(ROOT)/tests && ./run_smoke.sh

daily:
@cd $(ROOT)/tests && ./run_daily.sh

collect:
@cd $(ROOT)/tests && ./collect_logs.sh

clean:
@rm -rf $(ROOT)/artifacts/* $(ROOT)/tests/tmp 2>/dev/null || true
@mkdir -p $(ROOT)/artifacts
@echo "Artifacts directory reset"
