# Optional file associations

Passql does not currently install operating system associations, icons, MIME
types, or a VS Code extension. A `.psl` file is binary data, not SQL source.
Do not open its bytes as text or automatically register system settings.

An eventual installer may offer associations only after the user chooses to
install them. It should recognize the database by signature and format
metadata after opening, since file names can be changed. Associations alone
must not imply that every file ending in `.psl` is a valid database.

## Linux

A desktop package may ship a `shared-mime-info` XML type such as
`application/x-passql-database` with a `*.psl` glob and known signature
matches. MIME matching must account for the observed version 1, 2, and 3
signatures. Desktop launchers can reference an icon installed with that
package. Package installation or a user action should refresh the desktop MIME
cache; the database engine itself should not modify system MIME directories.

## macOS

A signed macOS application may export a Uniform Type Identifier for Passql
database documents, associate the `psl` extension, and provide a document
icon in its application bundle. The application should inspect the signature
before displaying metadata. Do not change Launch Services registrations
outside an explicit app installation or user action.

## Windows

A Windows installer may define a user selected ProgID for Passql database
files, associate the `.psl` extension, provide a description and application
icon, and optionally register a MIME content type. Use an installer with
clear consent and an uninstall path. Do not write registry associations from
the database library or CLI.

## VS Code

There is no VS Code extension. A future extension should register `.psl` as a
Passql database and use a custom binary editor or metadata view. It should not
associate the file with SQL syntax highlighting or expose arbitrary database
bytes as source text. The extension should inspect metadata without opening a
writer or invoking recovery.
