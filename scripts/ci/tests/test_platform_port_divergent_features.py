"""A port's divergent build must link the port it edits (#1898).

`vesper-port-divergent` builds an edited copy of a crate's Vesper port, and
scripts/test-port-harness.sh asserts that the crate's tests then fail. If the
crate still picks the Vesper or Rust implementation with
`#[cfg(feature = "vesper-port")]`, the divergent feature has to enable
`vesper-port`. Otherwise the edited object is built but never linked, the
tests run against the Rust reference, and the gate passes. #1639, #1671 and
#1699 dropped that implication when they made the port build unconditional.
"""
from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PORT_CFG = re.compile(r'feature\s*=\s*"vesper-port"')


def uses_port_cfg(crate: Path) -> bool:
    # Only the library picks an implementation; a test's own
    # `cfg!(feature = "vesper-port")` (expected outputs, blessing) does not.
    src = crate / "src"
    return src.is_dir() and any(
        PORT_CFG.search(f.read_text(encoding="utf-8")) for f in src.rglob("*.rs")
    )


class PortDivergentFeatureTests(unittest.TestCase):
    def test_divergent_feature_enables_the_port_where_the_port_is_cfg_gated(self):
        checked = 0
        for manifest in sorted(ROOT.glob("crates/*/Cargo.toml")):
            features = tomllib.loads(manifest.read_text(encoding="utf-8")).get("features", {})
            if "vesper-port-divergent" not in features or not uses_port_cfg(manifest.parent):
                continue
            checked += 1
            with self.subTest(crate=manifest.parent.name):
                self.assertIn(
                    "vesper-port",
                    features["vesper-port-divergent"],
                    f"{manifest.relative_to(ROOT)}: vesper-port-divergent must enable "
                    "vesper-port, or the divergent build tests the Rust reference",
                )
        self.assertGreater(checked, 0)


if __name__ == "__main__":
    unittest.main()
