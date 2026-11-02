# File: ballista/sdks/python/tests/test_bundle.py
import pytest

from ballista_client import Bundle, ConfigurationError
from ballista_client.bundle import valid_bundle_path

from .test_plan import shop_plan


def test_paths() -> None:
    for bad in ["", "/etc/passwd", "../x", "a/../b", ".env", "a//b", "a b", "inline/x.csv"]:
        assert not valid_bundle_path(bad), bad
    assert valid_bundle_path("proto/api.pb")


def test_payload_carries_structured_plan_and_assets() -> None:
    b = Bundle(shop_plan(), labels={"ci": "1"}).add_bytes("proto/api.pb", b"\x00\x01")
    payload = b.to_payload()
    assert isinstance(payload["plan"], dict) and payload["plan"]["name"] == "shop api"
    assert payload["files"] == {"proto/api.pb": "AAE="}
    assert payload["labels"] == {"ci": "1"}
    assert isinstance(Bundle('name = "x"').to_payload()["plan"], str)


def test_limits() -> None:
    b = Bundle({"name": "x"})
    with pytest.raises(ConfigurationError):
        b.add_bytes("plan.toml", b"")
    with pytest.raises(ConfigurationError):
        b.add_bytes("big.bin", b"\0" * (16 * 1024 * 1024 + 1))
    with pytest.raises(ConfigurationError):
        Bundle("   ")
    with pytest.raises(ConfigurationError):
        Bundle({"name": "x"}, labels={"bad key": "v"})
