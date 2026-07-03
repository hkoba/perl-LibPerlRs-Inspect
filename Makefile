BUILD = debug

SO = blib/arch/auto/OpTree/Analyzer/Analyzer.so
PM = blib/lib/OpTree/Analyzer.pm

all: $(SO) $(PM)

.PHONY: all cargo-build test clean

cargo-build:
	cargo build

target/$(BUILD)/libanalyzer_xs.so: cargo-build

$(SO): target/$(BUILD)/libanalyzer_xs.so
	@mkdir -p $(dir $@)
	cp $< $@

$(PM): perllib/OpTree/Analyzer.pm
	@mkdir -p $(dir $@)
	cp $< $@

test: all
	prove -b t/

clean:
	rm -rf blib
	cargo clean
