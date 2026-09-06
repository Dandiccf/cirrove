# Security

Cirrove has no supported stable release yet. The initial milestone is a development
metadata client, with no mount or upload implementation.

Do not post credentials, signed URLs, raw HTTP dumps or personal file metadata in
public issues. For a suspected vulnerability, contact the repository maintainer
through an available private GitHub channel. Private vulnerability reporting should
be enabled when the repository is published; do not assume it is enabled already.

Tokens and opaque cursors must not be logged. Credential storage and production
OAuth are a roadmap prerequisite, not a current capability. Developer bootstrap
tokens are supplied explicitly in private local files and are never persisted by
the application. The local index can contain sensitive filenames; its containing
directory must be private. There is no built-in at-rest encryption in this milestone.
