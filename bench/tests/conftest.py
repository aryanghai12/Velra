"""pytest configuration for the benchmark's own test suite."""


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "slow: takes more than a second. Deselect with `-m 'not slow'`.")
