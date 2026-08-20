# Written from invariant 5 of CLAUDE.md, in its own words: "`up` is idempotent
# and `down` is complete. Running `up` twice must not start a second copy or
# corrupt the pidfile, and `down` must leave nothing holding a port. The second
# run is the real test: works twice, from empty, untouched."
#
# The signalling half of that has had tests since 2026-08-09. This is the other
# half, and it needed a built stack, which is why it waited.
#
# These scenarios are bound to #[ignore] tests. They start real processes and
# bind ports 4100 and 8080, so `scripts/e2e.sh` runs them deliberately and
# `cargo test` does not. See that script for why it refuses rather than waits
# when a port is busy.

Feature: Bringing the stack up twice and down once
  As an operator who runs taipan up again without thinking about it
  I want the second one to refuse instead of starting a second copy
  So that I never end up with two money planes and one pidfile

  @test:up_is_idempotent_and_down_leaves_nothing_behind
  Scenario: up, up again, then down
    Given no taipan environment is running
    When I bring an environment up
    Then the gateway answers on 4100 and cloud answers on 8080
    And a pidfile and a descriptor are written

    When I bring the same environment up again
    Then it refuses, saying the environment already appears to be up
    And it tells me to run taipan down first
    And the pidfile is byte for byte what it was
    # Not "roughly the same". A refused up that rewrote the pidfile would be
    # corrupting the one file down depends on, which is the other half of what
    # invariant 5 forbids.

    When I bring it down
    Then nothing holds 4100 or 8080
    And the pidfile, keyfile and descriptor are gone

    When I bring it down again
    Then it says there is nothing to stop, and does not treat that as an error

    When I bring it up once more
    Then it comes up, because the second run from empty is the real test

  @test:a_stale_pidfile_does_not_block_a_fresh_up
  Scenario: A pidfile left behind by a machine that was rebooted
    Given a pidfile naming a process that no longer exists
    When I bring that environment up
    Then the stale pidfile is overwritten rather than treated as a live stack
    And the gateway comes up
    # The mirror of the scenario above, and the more dangerous one to get
    # wrong. Refusing here would mean an environment can never be restarted
    # after a crash or a reboot without deleting a file by hand, and the
    # message would tell the operator to run `taipan down`, which does nothing
    # for a process that is already gone.

  @test:the_gateway_never_starts_without_being_told_which_it_is
  Scenario: Real spend or invented spend, never unlabelled
    Given I did not pass --upstream
    When I bring an environment up
    Then the gateway is started with its stub explicitly enabled
    And the summary says the spend it reports is invented
    # tokenfuse refuses to start with neither TOKENFUSE_UPSTREAM nor
    # TOKENFUSE_ALLOW_STUB set, on the grounds that a stub metering a fixed
    # 1000 input / 500 output tokens as real spend invents both the answers and
    # the money. taipan set neither from 2026-07-25 until 2026-08-20, so `up`
    # simply did not work, and nothing said so because nothing ran it.
    #
    # The numbers travel: the descriptor this writes is what the Genaryx
    # console auto-discovers, and a person reads them there as money. So the
    # label is printed in the summary, not only in a log line.
