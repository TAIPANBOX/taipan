# Written from README.md, "What it starts" and "Requirements": taipan up
# "builds (or reuses) the service binaries from your sibling checkouts", and
# taipan "never modifies these repos; it only reads their source (to decide
# whether a rebuild is needed) and runs their own build tool".
#
# The reuse half is the half an operator feels. A release build of the tokenfuse
# workspace is minutes; doing it on every up would make the one-command promise
# a lie. Nothing tested that the skip actually skips.

Feature: Reusing what is already built
  As an operator running taipan up for the second time today
  I want a cached, fresh binary to be reused
  So that up is quick and does not rebuild a workspace that has not changed

  @test:a_fresh_cached_build_is_reused_without_shelling_out_to_cargo
  Scenario: Both binaries are cached and newer than the source
    Given cached gateway and cloud binaries in ~/.taipan/bin
    And a build marker newer than every source file in the sibling repo
    When taipan makes sure the binaries exist
    Then it returns the cached paths
    And it does not run cargo
    # The test proves "does not run cargo" by pointing the sibling repo at a
    # directory where a cargo build could not possibly succeed. If the skip
    # stopped skipping, the call would fail rather than quietly get slower,
    # which is the only way a test can see the difference.

  @test:a_missing_binary_is_not_treated_as_a_fresh_build
  Scenario: The marker is fresh but somebody deleted a binary
    Given a build marker newer than every source file
    And only one of the two binaries present in ~/.taipan/bin
    When taipan makes sure the binaries exist
    Then it does not report the pair as up to date
    # The marker and the binaries can disagree. Trusting the marker alone would
    # hand back a path to a file that is not there, and the failure would land
    # later, at spawn time, wearing a different name.

  @test:validate_name_rejects_path_traversal_and_empty
  Scenario: An operator passes a name that would escape the environments directory
    Given an environment name containing a path traversal
    When taipan validates it
    Then it is refused
    # Names become path segments under ~/.taipan/environments and bearer-key
    # org segments. This is already tested; the scenario is written down so the
    # reason survives next to the others rather than living only in a comment.

  @test:a_log_tail_is_returned_for_a_service_that_failed_to_start
  Scenario: A service never became healthy and its log is needed in the error
    Given a log file with more lines than the tail being asked for
    When taipan reads the tail for an error message
    Then it returns only the last lines
    And an unreadable log is reported as unreadable rather than aborting
    # This runs only on the failure path, which is the path least likely to be
    # exercised by hand and most likely to be read by an operator in a hurry.

  @test:touching_the_events_file_leaves_existing_content_alone
  Scenario: An events file already holds a previous run's events
    Given an events NDJSON file with content in it
    When taipan touches it before starting Idryx
    Then the file still holds that content
    # Idryx refuses to start without the file existing. Truncating it instead
    # would silently drop the history an operator is about to look at.
