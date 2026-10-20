# rustypdf developer commands. Run `just` to list them.

default:
    @just --list

# Format the code
fmt:
    cargo fmt

# Lint like CI does
lint:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings

# Run all tests
test:
    cargo test

# Run the service locally (templates/ and fonts/ from the repo)
run:
    TEMPLATES_DIR=templates FONTS_DIR=fonts cargo run

# Render one template without HTTP: just render hello templates/hello/sample.json
render template data out="out.pdf":
    FONTS_DIR=fonts cargo run -- render templates/{{template}} {{data}} -o {{out}}

# Re-approve golden PNGs (only on purpose; give the reason in the PR)
golden-update:
    UPDATE_GOLDEN=1 cargo test --test golden

# Render every sample.json and check it with veraPDF 3b
pdfa:
    scripts/check-pdfa.sh

# Build the Docker image
docker:
    docker build -t rustypdf:latest .

# Criterion render benchmarks (no HTTP); reports in target/criterion
bench *args:
    cargo bench --bench render -- {{args}}

# In-process soak: new invoice number per render; prints latency and RSS
soak seconds="60" threads="1":
    cargo run --release --example soak -- {{seconds}} {{threads}}

# k6 load test of the Docker image: warm, cold or soak (needs k6)
loadtest scenario="warm":
    scripts/loadtest.sh {{scenario}}
