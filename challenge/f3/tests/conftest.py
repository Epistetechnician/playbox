def pytest_configure(config):
    config.addinivalue_line("markers", "slow: re-trains a real candidate (about 30 s)")
