# Written from the keyfile module doc comment in src/keys.rs and the fix it
# now documents. The keyfile carries live Cloud/Wardryx bearer tokens; the
# old implementation wrote it at the umask default and only afterward chmod'd
# it to 0600, so for a moment, or forever if that chmod call itself errored,
# the tokens sat on disk at whatever the caller's umask allowed. up.rs's
# keyfile-failure branch also removed only the pidfile on that error, leaving
# the keyfile (at whatever permissions it ended up with) behind.

Feature: The keyfile never exists at looser than 0600
  As an operator on a machine other accounts can log into
  I want the keyfile's live bearer tokens to never sit at loose permissions
  So that another account cannot read them, even for a moment or after a failure

  @test:save_never_lets_a_second_reader_observe_a_looser_mode
  Scenario: A second reader races the keyfile write
    Given a fresh path under a permissive umask
    When KeyFile::save writes to it
    Then no reader ever observes the file at a mode other than 0600
    # Create-with-mode asks the OS to apply 0600 atomically as part of
    # creating the file, so there is no window to observe, unlike writing at
    # the umask default and chmod-ing afterward.

  @test:save_leaves_the_file_at_mode_0600_under_a_permissive_umask
  Scenario: The file's final mode, whatever the umask
    Given a fresh path under a permissive umask
    When KeyFile::save writes to it
    Then the file's mode is 0600

  @test:remove_pidfile_and_keyfile_removes_both_when_present
  Scenario: up's keyfile-failure branch cleans up both files
    Given a pidfile and a keyfile already on disk
    When the shared cleanup helper runs
    Then neither the pidfile nor the keyfile exists afterward
    # The keyfile-failure branch used to remove only the pidfile: a failed
    # `keyfile.save` could still leave the keyfile itself, with live bearer
    # tokens, on disk. `up::run` builds and spawns real sibling binaries, so
    # this scenario is bound to a test of the extracted cleanup helper rather
    # than to `run` itself; see that test's own doc comment.
