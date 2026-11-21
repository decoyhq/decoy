---
title: Glossary
description: Understand the names you see in Decoy mock files, requests, and tests.
---

## Route

A Route identifies the interaction you want Decoy to handle. For HTTP, that's a method and a path. In the [quickstart](quickstart.mdx), the Route named `users-by-id` handles `GET /users/{id}`. Both `/users/123` and `/users/456` fit that path.

## Case

A Case describes which inputs matter within a Route. The `user-123` Case checks the captured `id`, so only a request for `/users/123` matches it. Cases can also distinguish requests by their query, headers, or body. [See how Case matching works](guide/routes.md#case-match).

## Behavior

A Behavior tells Decoy what to do for a matching Case. The same `user-123` Case can have a `success` Behavior that returns Ada and a `missing` Behavior that returns a 404. Changing the active Behavior lets you try both outcomes with exactly the same request.

## Behavior address

A Behavior address names the Route, Case, and Behavior in that order. Read `users-by-id:user-123:missing` as “on the users-by-id Route, use the missing Behavior for the user-123 Case.”

## Collection

A Collection selects the Behaviors you want to use together. A test can switch from `happy-path` to `user-missing` in one call, even when the Collections select Behaviors across several Routes. [Write a Collection](guide/collections.md).

## Scenario

A Scenario is the situation you want your app to experience, such as a checkout with a missing user. The Collection is what you write to reproduce that situation.

## Session

A Session lets one test change responses without affecting another test using the same server. Send the same `x-mock-session` header on the control call and the request that should see the change. [Use Sessions in parallel tests](guide/sessions-and-control.md).

## Selection

The Selection is the active Collection plus any Behavior overrides for one Session. For example, a test can keep the `happy-path` Collection active and override just one Case to return a 404.

## Catalog

The Catalog contains the Routes, Cases, Behaviors, Collections, and settings that Decoy has loaded. Sessions share that Catalog and choose independently which Behaviors to activate.

## Fail-closed

When a request misses, Decoy returns a diagnostic. For example, an HTTP request to an unconfigured path gets a mock-miss response. Reaching a real upstream requires an explicit [Passthrough](#passthrough) Behavior.

## Passthrough

Passthrough is a Behavior that forwards a matched request to an upstream. You might mock `GET /users/123` while forwarding requests for other users to a development service. [Configure a Passthrough Behavior](guide/routes.md#passthrough-behavior).

## Order-based matching

Decoy checks the active Cases from the bottom of a Collection upward. If a general Case and a more specific one both match, the one listed later wins. [See how Collection order works](guide/collections.md).

## Request envelope

The request envelope contains the values a Case or template can read from one request. For `GET /users/123`, the HTTP envelope includes the path and the captured ID. A gRPC envelope includes the method and the decoded message.

## Message envelope

On an open WebSocket, each incoming message has its own envelope. A Case can use the decoded message together with information from the connection, such as its path or query. [Match WebSocket messages](guide/websocket.md).

## Template context

The template context supplies values while Decoy builds a response. A response body can include the ID captured from the incoming path. [Write a response template](guide/routes.md#templates).

## Standard function

A standard function is built into Decoy's expression language. For example, `uuid()` can generate a request ID inside a response template.

## Controller

The Controller changes a Session's Selection. Use `useCollection` to switch a whole scenario, or `useRoute` to select a single Behavior.

## Control API

The Control API exposes runtime operations over HTTP. For example, posting to `/__decoy__/control/useCollection` switches the Collection for the Session named in the request. [See the Control API requests](guide/sessions-and-control.md#control-api).

## Router

A Router is the handle a test uses to control its own Session. In Playwright, for example, `decoy.useCollection('happy-path')` changes the responses for that test. [Use the Playwright fixture](guide/playwright.md).

## Connection

A Connection handle represents the live connections for a Route in one Session. Once a browser opens its WebSocket, a test can use the handle to push a message to the connected client. [Send a message from a test](guide/sessions-and-control.md#send).

## Pipeline

A Pipeline lists the transforms to run on a transport payload. For example, a WebSocket Pipeline can decode binary input before a Case matches it. A named Pipeline has separate input and output lists. [Configure a Pipeline](guide/transformer-pipelines.md).

## Transformer

A Transformer takes one value and produces another. It can change the format, the contents, or both. For example, one step might decompress bytes while the next parses them into an object. Another Transformer might take an object and return just its `id`. Each step passes its result to the next. A Transformer can run as a command or a persistent worker. [Write a worker with the TypeScript SDK](../../../packages/plugin/README.md) or [implement the worker protocol](../../../docs/reference/plugin-protocol.md).

## Pipeline step

A Pipeline step is one use of a Transformer in an input or output list, or a built-in gRPC transformation. Its position determines which value it receives from the previous step.
