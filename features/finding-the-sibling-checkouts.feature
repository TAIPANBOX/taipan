# Written from README.md, "Requirements": taipan expects sibling checkouts of
# tokenfuse (always) and, with --with, wardryx and Idryx, "either next to your
# taipan checkout, or next to its parent directory. taipan tries both locations
# before giving up; pass --workspace <dir> to point at a different parent
# directory entirely."
#
# Every scenario is bound to a test by its @test: tag. scripts/scenarios-have-tests.sh
# refuses a push where a tag names a test that does not exist.

Feature: Finding the sibling checkouts
  As an operator running taipan from wherever my checkouts happen to live
  I want taipan to find the other TAIPANBOX repos on its own
  So that one command works without me spelling out four paths

  @test:finds_a_sibling_directly_under_the_workspace_root
  Scenario: The repos sit beside each other under one parent
    Given a workspace directory that contains a "tokenfuse" directory
    When taipan looks for the "tokenfuse" checkout
    Then it returns that directory

  @test:finds_a_sibling_under_the_workspace_parent
  Scenario: taipan is run from inside its own checkout
    Given a workspace directory whose PARENT contains a "tokenfuse" directory
    And the workspace directory itself does not
    When taipan looks for the "tokenfuse" checkout
    Then it returns the parent's copy
    # This is the case the README calls "run taipan up from inside the taipan
    # checkout". It is the ordinary one, and it was the one with no test.

  @test:the_workspace_root_wins_over_the_parent
  Scenario: A checkout exists in both places
    Given a "tokenfuse" directory under the workspace root
    And another "tokenfuse" directory under the workspace parent
    When taipan looks for the "tokenfuse" checkout
    Then it returns the one under the root
    # Order matters and nothing said so. An operator who puts a checkout beside
    # taipan is overriding whatever sits one level up, and would be surprised to
    # be given the other.

  @test:case_variants_are_tried_in_the_order_given
  Scenario: The repo is checked out under a different capitalisation
    Given a workspace directory that contains a "Wardryx" directory
    And no "wardryx" directory anywhere
    When taipan looks for the "wardryx" checkout, trying "wardryx" then "Wardryx"
    Then it returns the directory that exists on disk
    # Idryx is capitalised on disk and lowercase in its module path. The
    # candidate list exists for exactly that, and had no test.
    #
    # KNOWN LIMIT, and it is not a small one. The default macOS filesystem is
    # case insensitive, so the FIRST candidate already resolves when only the
    # capitalised directory exists. On a Mac this scenario cannot go red: break
    # the loop so it tries one candidate and it still passes. It is a real test
    # of the candidate list on Linux only. Measured 2026-08-20 by asserting the
    # returned path equalled the capitalised name, which failed on this Mac and
    # would have passed on Linux. There is no CI in this repository, so a
    # platform-dependent test here is checked by whoever runs the hook, and
    # that is a Mac. Compare the fifth shape of the verification gap:
    # the instrument is not the environment.

  @test:a_file_named_like_the_repo_is_not_a_checkout
  Scenario: Something with the right name is not a directory
    Given a workspace directory that contains a FILE called "tokenfuse"
    When taipan looks for the "tokenfuse" checkout
    Then it does not accept the file
    And it reports the checkout as not found

  @test:not_found_names_every_path_tried_and_points_at_the_flag
  Scenario: No checkout exists anywhere taipan looked
    Given a workspace with no sibling checkouts at all
    When taipan looks for the "tokenfuse" checkout
    Then the error names every path it tried
    And the error tells the operator about --workspace
    # An error that says only "not found" leaves the operator guessing which
    # two directories were searched.
