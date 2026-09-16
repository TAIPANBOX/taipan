# Written from the keyfile module doc comment in src/keys.rs and the fix it
# now documents. The keyfile carries live Cloud/Wardryx bearer tokens; the
# old implementation wrote it at the umask default and only afterward chmod'd
# it to 0600, so for a moment, or forever if that chmod call itself errored,
# the tokens sat on disk at whatever the caller's umask allowed. up.rs's
# keyfile-failure branch also removed only the pidfile on that error, leaving
# the keyfile (at whatever permissions it ended up with) behind.
#
# Two more scenarios below came from a Fable review of the fix itself: an
# EXISTING file at the path kept its old mode until the trailing chmod ran
# (the same loose window, reachable a second way), and a symlink at the path
# was followed rather than refused. The cleanup scenario's Then clause was
# widened to include the descriptor, for the matching gap on that branch.

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

  @test:save_replaces_an_existing_file_rather_than_reusing_its_inode
  Scenario: An existing file at the path is replaced, not reused
    Given a stale file already at the path, at mode 0644
    When KeyFile::save writes to it
    Then the file's final mode is 0600
    And the file is a different inode than the stale one
    # Opening an existing inode with create-and-truncate ignores the mode
    # argument (open(2) only applies it when a new inode is created), so the
    # new tokens were written into the stale file at its OLD mode before the
    # trailing chmod narrowed it: the same loose window this feature exists
    # to close, reachable a second way. Removing anything already at the
    # path before creating closes it for an existing plain file.

  @test:save_refuses_a_symlink_at_the_path_rather_than_following_it
  Scenario: A symlink at the path is refused, not followed
    Given a symlink at the path pointing elsewhere
    When KeyFile::save writes to it
    Then the write fails with AlreadyExists
    And the symlink's target is left untouched
    # create_new (O_CREAT | O_EXCL) never opens through an existing
    # directory entry, symlink or not, so the fix for the scenario above
    # closes this one for free: the path is left alone rather than deleted
    # or written through.

  @test:remove_pidfile_and_keyfile_removes_both_when_present
  Scenario: up's keyfile- and descriptor-failure branches clean up every file
    Given a pidfile, a keyfile, and a descriptor already on disk
    When the shared cleanup helper runs
    Then neither the pidfile, the keyfile, nor the descriptor exists afterward
    # The keyfile-failure branch used to remove only the pidfile: a failed
    # `keyfile.save` could still leave the keyfile itself, with live bearer
    # tokens, on disk. The descriptor-failure branch then still left a
    # partial descriptor behind on a failed `descriptor.save` (Genaryx
    # auto-discovers it as garbage rather than as an absent environment).
    # `up::run` builds and spawns real sibling binaries, so this scenario is
    # bound to a test of the extracted cleanup helper rather than to `run`
    # itself; see that test's own doc comment.
