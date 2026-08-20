# Written from README.md, "The descriptor", and from invariant 4 in CLAUDE.md:
# "The descriptor is a cross-repo contract. Genaryx auto-discovers it. A field
# rename or a path change is a coordinated change with that repo, and its
# failure mode is silence, not an error."
#
# Silence is the whole point. Nothing in this repository fails when a field is
# renamed; a console in a different repository simply finds nothing and shows
# an empty screen. Invariant 4 was carried by prose alone until these tests.

Feature: The descriptor other tools discover
  As the Genaryx console auto-discovering a running stack
  I want the descriptor's field names to be exactly what I was built to read
  So that a rename here does not leave me showing an empty screen

  @test:the_descriptor_carries_exactly_the_documented_field_names
  Scenario: The money plane is up and the descriptor is written
    Given an environment with a gateway and a cloud service
    When the descriptor is serialised
    Then it carries the keys "name", "created_at", "host", "services", "events" and "keys"
    And the gateway entry carries "url" and "mode"
    And the cloud entry carries "url"

  @test:an_absent_optional_service_leaves_no_null_behind
  Scenario: Only the money plane was started
    Given an environment started without --with
    When the descriptor is serialised
    Then no "wardryx" or "idryx" entry appears at all
    And no key reference for them appears as a null
    # A null is not the same as absent to a consumer that checks for presence.

  @test:a_service_that_failed_is_named_in_unavailable_with_a_reason
  Scenario: An optional service failed to build
    Given wardryx was requested with --with but could not start
    When the descriptor is serialised
    Then "unavailable" names wardryx
    And it carries a plain-text reason
    # The README's promise is that up "degrades gracefully rather than failing
    # the whole environment over an optional piece, and never omits a failure
    # silently". An omission and a graceful degrade look the same on screen
    # unless the reason is written down.

  @test:an_environment_with_nothing_unavailable_omits_the_section
  Scenario: Everything requested came up
    Given an environment where every requested service started
    When the descriptor is serialised
    Then no empty "unavailable" object is written
    # Additive fields are tolerated by consumers, but an empty object invites a
    # reader to treat "unavailable exists" as "something is unavailable".

  @test:the_descriptor_round_trips_through_the_file_it_is_written_to
  Scenario: A consumer reads back what taipan wrote
    Given a descriptor written to a path whose directory does not exist yet
    Then the directory is created
    And reading the file back yields the same descriptor
