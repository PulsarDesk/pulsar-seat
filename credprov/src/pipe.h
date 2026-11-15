// SPDX-License-Identifier: GPL-3.0-only
// Client of the Pulsar Seat service control pipe (\\.\pipe\PulsarSeat.Control) for the
// credential provider. Frame = u32 LE length + UTF-8 JSON (architecture.md §5).
//
// Every call opens its own connection, says hello as `credential_provider`, sends one
// request and closes. Short time limits (LogonUI must never hang on us): connect 500 ms,
// each request 2 s. The server must be the running PulsarSeat service (server PID checked
// against the service control manager), so a process that grabbed the pipe name cannot
// feed us credentials or hide tiles.
#pragma once

#include "common.h"

namespace pulsar {

enum class Scenario { Logon, Unlock };

// Answer of credprov_query. `reachable == false` = the service did not answer: hide
// nothing, show nothing.
struct SessionInfo {
    bool reachable = false;
    int seat = 0;  // 0 = not a seat session
    bool autoSubmit = false;
    bool hideOthers = false;
    std::wstring account;  // DOMAIN\user (no secret)
    std::wstring userSid;
    std::vector<int> switchTargets;
    // Logon only: a fast_user_switch hand-over to this session is under way and the grant
    // is not armed yet; ask again shortly (Provider polls, bounded).
    bool pending = false;
    // A password-less switch handed the console to this logon screen for `seat`: its tile
    // is the default (selected) one, without auto-logon.
    bool preselect = false;
};

// Same answer for the tiles and the filter (pending is not compared).
bool SameSession(const SessionInfo& a, const SessionInfo& b);

struct SeatCredential {
    std::wstring domain;
    std::wstring user;
    SecretW password;
};

class ServiceClient {
public:
    // credprov_query
    static SessionInfo Query(Scenario scenario);
    // credprov_take_credentials (once per armed session; false when refused / unreachable)
    static bool TakeCredentials(SeatCredential& out);
    // credprov_report
    static void Report(LONG status, LONG subStatus);
    // credprov_switch
    static bool Switch(int seat);
};

}  // namespace pulsar
