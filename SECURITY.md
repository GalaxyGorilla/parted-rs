# Security policy

## Reporting

Please report vulnerabilities privately through GitHub's **Report a
vulnerability** function for this repository. Do not open a public issue before
a fix is available. Include the affected command, target type, reproduction
steps, and whether any partition-table write occurred.

## Supported versions

Until the first stable release, only the latest commit on `main` receives
security fixes. Published releases will document any longer support window.

## Operational boundary

`parted` edits partition-table metadata and cannot replace a verified backup of
partition contents. Test reports on disposable images whenever possible. Never
attach confidential disk images to a report; provide a minimal synthetic
reproducer instead.
