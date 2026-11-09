# Notes about migration scripts

This document aims at listing all tips and errors we made in the past with the
migration scripts, with the hope that future us won't replicate these errors.

## Tips

1. Determining the up-to-date schema after all the migrations can be hard to
   track by hand. Here is the trick: create a temporary database, apply all the
   migration scripts, and then query the database, like so:

   ```shell
   $ cd <migration-directory>;
   $ for i in $(/bin/ls -1); sqlite3 temporary.db < $i; end
   $ sqlite3 temporary.db '.schema'
   $ # Enjoy
   ```

2. Writing comments in SQL is great! Every comments outside tables are not part
   of the schema, but all comments inside the tables _are_ part of the schema.
   It means that if a column has a comment, it will be displayed with the
   `.schema` command for example. However, if one needs to add a comment for a
   table (which is very recommended), the comment must be inside the table, like
   so:

   ```sql
   CREATE TABLE IF NOT EXISTS "foo" (
      --- This table exists to address this and that.

      --- First column!
      foo BLOB NOT NULL
   );
   ```

## Errors

1. _Identifiers_ can be delimited by double-quotes, while _string literals_ must
   be delimited by single-quotes. Even if SQLite has a compile-time
   configuration to consider double-quotes to be string literals delimiters,
   this configuration can be disabled, and would create an error.

   The following example is correct:

   ```sql
   CREATE TABLE "foo";
   SELECT id FROM foo WHERE id = 'hello';
   ```

   But the following example is incorrect:

   ```sql
   CREATE TABLE 'foo';
   SELECT id FROM foo WHERE id = "hello";
   ```

   See
   [the _Double-quoted String Literals Are Accepted_ Section][quirks-double-quoted-string-literals]
   in the SQLite documentation to learn more.

[quirks-double-quoted-string-literals]: https://sqlite.org/quirks.html#double_quoted_string_literals_are_accepted
