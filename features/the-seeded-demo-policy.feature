# Written from README.md, "What it starts": --with wardryx seeds "a demo policy
# scoped to agent://mockryx.local/* (a small require_human_above_usd and a
# deny_tool: [shell_exec])", and from the doc comment on DEMO_POLICY_YAML, which
# states the safety property in its own words: the policy is scoped to the
# mockryx fire-drill rehearsal identities "so it never governs an operator's own
# agent traffic".
#
# That sentence is the reason these scenarios exist. A policy that widened its
# target would start deciding on real traffic on somebody's machine, and would
# do it quietly, because a policy that matches more is not an error.

Feature: The demo policy taipan seeds for Wardryx
  As an operator running taipan up --with wardryx
  I want the seeded policy to govern only the rehearsal identities
  So that a smoke-test policy never starts holding or denying my own agents

  @test:the_demo_policy_targets_only_the_mockryx_rehearsal_identities
  Scenario: The seeded policy cannot reach the operator's own agents
    Given the demo policy taipan seeds
    Then every rule in it targets "agent://mockryx.local/*"
    And no rule targets a wider glob

  @test:the_demo_policy_holds_costly_actions_and_denies_shell_exec
  Scenario: The seeded policy actually decides something
    Given the demo policy taipan seeds
    Then it holds an action above a small dollar amount for a human
    And it denies the "shell_exec" tool outright
    # The default before this policy existed was zero policies, which means
    # every request allowed and no secret to sign an approval with. A stack
    # that decides nothing looks identical to a stack that allows everything.

  @test:seeding_the_policy_replaces_an_earlier_run_rather_than_appending
  Scenario: taipan up runs a second time
    Given a policy file left behind by an earlier run
    When taipan seeds the demo policy again
    Then the file holds the demo policy once
    And nothing from the earlier run survives in it
    # The second run is the real test. An appended file would grow a duplicate
    # rule set on every up, and duplicated rules are how a deny quietly becomes
    # ambiguous.

  @test:seeding_the_policy_creates_the_directory_it_needs
  Scenario: The environment directory does not exist yet
    Given a policy path inside a directory that has never been created
    When taipan seeds the demo policy
    Then the directory is created
    And the policy is written into it

  @test:the_wardryx_port_is_the_one_the_readme_publishes
  Scenario: The port an operator reads in the README
    Given the README publishes Wardryx on port 8090
    Then taipan starts it on that port
    # The README's port table is a promise, and the descriptor consumers read
    # is built from these constants.
