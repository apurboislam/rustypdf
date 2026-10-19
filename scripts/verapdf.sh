#!/usr/bin/env bash
# Checks PDFs for PDF/A-3b compliance with veraPDF.
# Uses `verapdf` from PATH if present; otherwise fetches the veraPDF CLI from
# Maven Central (needs java + mvn) into a cache dir on first use.
# Usage: scripts/verapdf.sh file.pdf...   (exit code 1 if any file fails)
set -euo pipefail

if command -v verapdf >/dev/null 2>&1; then
  exec verapdf --flavour 3b --format text "$@"
fi

VERSION=1.28.2
DIR="${VERAPDF_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/rustypdf/verapdf-$VERSION}"
if [ ! -d "$DIR/lib" ]; then
  mkdir -p "$DIR"
  cat > "$DIR/pom.xml" <<POM
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>local</groupId><artifactId>verapdf-runner</artifactId><version>1</version>
  <dependencies>
    <dependency>
      <groupId>org.verapdf.apps</groupId><artifactId>greenfield-apps</artifactId><version>$VERSION</version>
    </dependency>
  </dependencies>
</project>
POM
  mvn -q -f "$DIR/pom.xml" dependency:copy-dependencies -DoutputDirectory="$DIR/lib.tmp" >&2
  mv "$DIR/lib.tmp" "$DIR/lib"
fi
exec java -cp "$DIR/lib/*" org.verapdf.apps.GreenfieldCliWrapper --flavour 3b --format text "$@"
