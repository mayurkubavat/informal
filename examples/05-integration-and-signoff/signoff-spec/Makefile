REPO_ROOT ?= ../../..
INFORMAL  ?= $(REPO_ROOT)/target/release/informal
ENGINES   ?= $(REPO_ROOT)/config/engines.toml
WORKDIR   ?= $(REPO_ROOT)/informal_work/lab05

.PHONY: fpv-lint fpv-run fpv-cert clean

fpv-lint:
	$(INFORMAL) shell --check run.jg.tcl

fpv-run: fpv-lint
	$(INFORMAL) --isolate --mem-gb 4 shell run.jg.tcl \
		--engines $(ENGINES) \
		--workdir $(WORKDIR)

fpv-cert: fpv-lint
	$(INFORMAL) --isolate --mem-gb 4 shell run.jg.tcl \
		--engines $(ENGINES) \
		--workdir $(WORKDIR) \
		--certificate $(WORKDIR)/certs
	python3 -I $(REPO_ROOT)/scripts/certificate_check.py $(WORKDIR)/certs

clean:
	rm -rf $(WORKDIR)
