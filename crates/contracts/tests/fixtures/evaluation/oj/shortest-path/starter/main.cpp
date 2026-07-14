#include <bits/stdc++.h>
using namespace std;

using int64 = long long;
const int64 INF = numeric_limits<int64>::max() / 4;

int main() {
  ios::sync_with_stdio(false);
  cin.tie(nullptr);

  int n, m, s, t;
  if (!(cin >> n >> m >> s >> t)) {
    return 0;
  }

  vector<vector<pair<int, int64>>> graph(n + 1);
  for (int i = 0; i < m; ++i) {
    int u, v;
    int64 w;
    cin >> u >> v >> w;
    graph[u].emplace_back(v, w);
  }

  vector<int64> dist(n + 1, INF);
  dist[s] = 0;

  using State = pair<int64, int>;
  priority_queue<State, vector<State>, greater<State>> pq;
  pq.emplace(0, s);

  while (!pq.empty()) {
    auto [d, u] = pq.top();
    pq.pop();
    if (d != dist[u]) {
      continue;
    }
    for (auto [v, w] : graph[u]) {
      if (dist[u] + w < dist[v]) {
        dist[v] = dist[u] + w;
        pq.emplace(dist[v], v);
      }
    }
  }

  if (dist[t] == INF) {
    cout << -1 << '\n';
  } else {
    cout << dist[t] << '\n';
  }

  return 0;
}
