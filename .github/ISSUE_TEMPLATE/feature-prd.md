---
name: Feature PRD
about: Specify a feature, its user-visible behavior, and how to verify it.
title: 'PRD: '
---

## Problem Statement

Who encounters the problem? Describe what they are trying to do and what happens today.

## Solution

Describe what that person will be able to do once this ships.

## User Stories

1. As a [role], I want [capability], so that [benefit].

Add a numbered story for each distinct workflow or important edge case. Describe an observable outcome for each.

## Implementation Decisions

Record agreed behavior, affected interfaces, and constraints. Link relevant ADRs. Prefer stable
concepts and contracts over file paths. Include a short prototype excerpt only if it captures a decision more precisely than prose.

## Testing Decisions

Which public entry point exercises the behavior? Describe the inputs, observable outcomes, and error cases. Link similar tests and identify the components they cover.

## Out of Scope

Which related requests will be handled separately?

## Further Notes

List unresolved questions, dependencies, and related issues. Omit this section if empty.
