# P13.1 real provider probe

This project intentionally contains only provider configuration and the two
accepted P13.1 data sources. It is not a lifecycle or compatibility fixture.

The harness copies it to a disposable directory before running OpenTofu, so
provider installation and lockfile creation never modify the repository.
