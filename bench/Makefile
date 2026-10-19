

PERF=sudo perf stat -r 1000 -e instructions:u,instructions:k -j --log-fd 1
LONG=this-is-a-directory-with-a-very-long-name-that-i-am-getting-tired-of-typing-by-hand-and-i-should-have-automated
BRANCHES=master stub-no-main lexer-runs

perfs: $(patsubst %, tz-perf-%, $(BRANCHES) bare)

.SECONDARY:

trueZ:
	nasm -f elf64 $@.asm && ld $@.o -o $@

short/ $(LONG)/ build/:
	-mkdir $@

short/trueZ $(LONG)/trueZ: trueZ short/ $(LONG)/
	cp $< $@

trueZ-%-short: aliasmux-stub-% short/trueZ
	aliasmux new -o . --stub $< $@ ./short/trueZ

trueZ-%-long: aliasmux-stub-% $(LONG)/trueZ
	aliasmux new -o . --stub $< $@ ./$(LONG)/trueZ

tz-perf-%: trueZ-%-short trueZ-%-long
	$(PERF) ./trueZ-$*-short | tee $@-short.perf.json
	$(PERF) ./trueZ-$*-long | tee $@-long.perf.json

tz-perf-bare: trueZ
	$(PERF) ./trueZ | tee $@.perf.json


.PHONY: FORCE
FORCE:

# The commit NAME points to. Checked on every run, but only rewritten when it
# changes, so whatever depends on it only rebuilds then.
build/rev-%: FORCE | build/
	@rev=$$(git -C .. rev-parse --verify --quiet '$*^{commit}') || { echo "$*: not a commit" >&2; exit 1; }; \
	[ "$$(cat $@ 2>/dev/null)" = "$$rev" ] || echo "$$rev" > $@

# Without this, make deletes the rev files as intermediates after each run.
.PRECIOUS: build/rev-%

aliasmux-stub-%: BDIR = build/build-$*
aliasmux-stub-%: build/rev-% | build/
	mkdir -p $(BDIR) && git -C .. archive $$(cat $<) | tar -x -C $(BDIR)
	$(MAKE) -C $(BDIR) build-static && cp $(BDIR)/target/x86_64-unknown-linux-musl/release/aliasmux-stub $@


no-pie:
	$(MAKE) perfs RUSTFLAGS="-C relocation-model=static" -B
	for i in *.json; do aliasmux eval 'mv {1} {1:.}{:N}' "$$i" || exit; done
	for i in *.perf; do aliasmux eval 'mv {1} {1:.}-no-pie.perf.json{:N}' "$$i" || exit; done

no-pie-compare: clean
	$(MAKE) -B no-pie perfs
	rm -f *bare-no-pie*.json
	./perfdiff.py

clean:
	-rm trueZ-* aliasmux-stub-*
	-rm -r short $(LONG)
	-rm *.json