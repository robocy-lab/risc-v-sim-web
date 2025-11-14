Use Case: list submissions from particular user
Actor: student
Goal: retrieve ULIDs of all submissions made by the actor

Preconditions:
- Actor is authorized through GitHub OAuth
- Actor has non-zero amount of submissions

Main Flow:
1. Actor sends GET request to /api/submissions
2. System returns a list of actor's submissions
3. System exposes endpoints for retrieving artefacts and simulation results for each returned submissions

Postconditions:
- Actor has the list of their submissions
