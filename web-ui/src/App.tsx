/*
 *  Copyright (C) 2024 Michael Bachmann
 *
 *  This program is free software: you can redistribute it and/or modify
 *  it under the terms of the GNU Affero General Public License as published by
 *  the Free Software Foundation, either version 3 of the License, or
 *  (at your option) any later version.
 *
 *  This program is distributed in the hope that it will be useful,
 *  but WITHOUT ANY WARRANTY; without even the implied warranty of
 *  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *  GNU Affero General Public License for more details.
 *
 *  You should have received a copy of the GNU Affero General Public License
 *  along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

import Paper from "@mui/material/Paper";
import Stack from "@mui/material/Stack";
import DiscoveryList from "./components/create_sender/DiscoveryList";

function App() {
  return (
    <Stack alignItems="center" justifyContent="center">
      <Paper>
        <DiscoveryList />
      </Paper>
    </Stack>
  );
}

export default App;
